use crate::mutator::{
    failure_mutator_descriptor, BasicMutator, MutatorActuator, MutatorActuatorDescriptor,
};
use auxon_sdk::{
    auth_token::AuthToken,
    mutation_plane::{
        protocol::{LeafwardsMessage, RootwardsMessage, MUTATION_PROTOCOL_VERSION},
        types::{AttrKv, AttrKvs, ParticipantId},
    },
    mutation_plane_client::parent_connection::MutationParentConnection,
};
use clap::Parser;
use std::env;
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::mpsc};
use url::Url;

mod mutator;

#[derive(Parser, Debug, Clone)]
#[command(version)]
struct Opts {
    /// Address to bind to
    #[arg(long, default_value = "127.0.0.1:9785")]
    addr: String,
}

const MUTATION_PROTOCOL_PARENT_URL_ENV_VAR: &str = "MUTATION_PROTOCOL_PARENT_URL";
const MUTATION_PROTOCOL_PARENT_URL_DEFAULT: &str = "modality-mutation://127.0.0.1:14192";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match do_main().await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!("{}", e);
            Err(e)
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct DeviantIds {
    mutator_id: String,
    mutation_id: String,
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let opts = Opts::parse();

    let mut_plane_pid = ParticipantId::allocate();
    let mut_url = mutation_proto_parent_url().expect("Mutation protocol parent URL");
    let auth_token = AuthToken::load().expect("Auth token for mutation client");
    let allow_insecure_tls = true;
    tracing::info!(url = %mut_url, "Connection to mutation plane");
    let mut mut_plane_conn =
        MutationParentConnection::connect(&mut_url, allow_insecure_tls).await?;
    mut_plane_conn
        .write_msg(&RootwardsMessage::ChildAuthAttempt {
            child_participant_id: mut_plane_pid,
            version: MUTATION_PROTOCOL_VERSION,
            token: auth_token.as_ref().to_vec(),
        })
        .await?;

    let auth_outcome = mut_plane_conn.read_msg().await?;
    match auth_outcome {
        LeafwardsMessage::ChildAuthOutcome {
            child_participant_id,
            version: _,
            ok,
            message,
        } => {
            if child_participant_id == mut_plane_pid {
                if !ok {
                    return Err(format!("Mutation plane authorization failed. {message:?}").into());
                }
            } else {
                return Err(
                    "Mutation plane auth outcome received for a different participant"
                        .to_string()
                        .into(),
                );
            }
        }
        resp => {
            return Err(format!("Mutation plane unexpected auth response. Got {resp:?}").into())
        }
    }

    let (tx, mut rx) = mpsc::channel(32);

    let tcp_task_join_handle = tokio::spawn(async move {
        tracing::info!(addr = opts.addr, "Listening");
        let listener = TcpListener::bind(opts.addr).await.unwrap();
        loop {
            let (mut socket, client_addr) = listener.accept().await.unwrap();
            tracing::info!(client = %client_addr, "Client connected");
            let ids = match rx.recv().await {
                Some(msg) => msg,
                None => return,
            };
            let msg = serde_json::to_string(&ids).unwrap();
            socket.write_all(msg.as_bytes()).await.unwrap();
        }
    });

    let mut_plane_task_join_handle = tokio::spawn(async move {
        let mut server = MutatorServer::new(mut_plane_pid, mut_plane_conn, tx);
        server.run().await;
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("User signaled shutdown");
        }
        _ =  mut_plane_task_join_handle => {
            tracing::warn!("Mutator server returned unexpectedly");
        }
        _ = tcp_task_join_handle => {
            tracing::warn!("TCP server returned unexpectedly");
        }
    };

    Ok(())
}

struct MutatorServer {
    mut_plane_pid: ParticipantId,
    mut_plane_conn: MutationParentConnection,
    mutator: BasicMutator,
    sender: mpsc::Sender<DeviantIds>,
}

impl MutatorServer {
    pub fn new(
        mut_plane_pid: ParticipantId,
        mut_plane_conn: MutationParentConnection,
        sender: mpsc::Sender<DeviantIds>,
    ) -> Self {
        let mutator = BasicMutator::new(failure_mutator_descriptor());
        Self {
            mut_plane_pid,
            mut_plane_conn,
            mutator,
            sender,
        }
    }

    pub async fn register_mutator(&mut self) {
        let announcement = mutator_announcement(self.mut_plane_pid, &self.mutator);
        self.mut_plane_conn.write_msg(&announcement).await.unwrap();
    }

    pub async fn run(&mut self) {
        self.register_mutator().await;
        loop {
            let msg = self.mut_plane_conn.read_msg().await.unwrap();
            self.handle_msg(msg).await;
            if let Some(mutation_id) = self.mutator.active_mutation() {
                let ids = DeviantIds {
                    mutator_id: self.mutator.mutator_id().to_string(),
                    mutation_id: mutation_id.to_string(),
                };
                self.sender.send(ids).await.unwrap();
                self.mutator.reset();
            }
        }
    }

    async fn handle_msg(&mut self, msg: LeafwardsMessage) {
        match msg {
            LeafwardsMessage::RequestForMutatorAnnouncements {} => {
                tracing::info!("Announcing mutator");
                let announcement = mutator_announcement(self.mut_plane_pid, &self.mutator);
                self.mut_plane_conn.write_msg(&announcement).await.unwrap();
            }
            LeafwardsMessage::NewMutation {
                mutator_id,
                mutation_id,
                maybe_trigger_mask: _,
                params,
            } => {
                if mutator_id == self.mutator.mutator_id() {
                    let params = params
                        .0
                        .into_iter()
                        .map(|kv| (kv.key.into(), kv.value))
                        .collect();
                    tracing::info!(mutator_id = %mutator_id, mutation_id = %mutation_id, "Injecting mutation");
                    self.mutator.inject(mutation_id, params);
                } else {
                    tracing::warn!(mutator_id = %mutator_id, "Failed to handle new mutation, mutator not hosted by this client");
                }
            }
            LeafwardsMessage::ClearSingleMutation {
                mutator_id,
                mutation_id,
                reset_if_active: _,
            } => {
                tracing::info!(mutator_id = %mutator_id, mutation_id = %mutation_id, "Ignoring request to clear mutation");
            }
            LeafwardsMessage::ClearMutationsForMutator {
                mutator_id,
                reset_if_active: _,
            } => {
                tracing::info!(mutator_id = %mutator_id, "Ignoring request to clear mutations");
            }
            LeafwardsMessage::ClearMutations {} => {
                tracing::info!("Ignoring request to clear all mutations");
            }
            msg => tracing::warn!(
                message = msg.name(),
                "Ignoring mutation plane leafwards message"
            ),
        }
    }
}

fn mutator_announcement<M: MutatorActuatorDescriptor + ?Sized>(
    participant_id: ParticipantId,
    m: &M,
) -> RootwardsMessage {
    let mutator_attrs = m
        .get_description_attributes()
        .map(|(k, value)| AttrKv {
            key: k.to_string(),
            value,
        })
        .collect();
    RootwardsMessage::MutatorAnnouncement {
        participant_id,
        mutator_id: m.mutator_id(),
        mutator_attrs: AttrKvs(mutator_attrs),
    }
}

#[derive(Debug, thiserror::Error)]
enum MutationProtocolUrlError {
    #[error(
        "The MUTATION_PROTOCOL_PARENT_URL environment variable contained a non-UTF-8-compatible string"
    )]
    EnvVarSpecifiedMutationProtoParentUrlNonUtf8,

    #[error("Mutation protocol parent URL error")]
    MutationProtoParentUrl(#[from] url::ParseError),
}

fn mutation_proto_parent_url() -> Result<Url, MutationProtocolUrlError> {
    match env::var(MUTATION_PROTOCOL_PARENT_URL_ENV_VAR) {
        Ok(val) => Ok(Url::parse(&val)?),
        Err(env::VarError::NotUnicode(_)) => {
            Err(MutationProtocolUrlError::EnvVarSpecifiedMutationProtoParentUrlNonUtf8)
        }
        Err(env::VarError::NotPresent) => Ok(Url::parse(MUTATION_PROTOCOL_PARENT_URL_DEFAULT)?),
    }
}
