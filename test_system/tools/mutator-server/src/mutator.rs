use auxon_sdk::{
    api::{AttrKey, AttrType, AttrVal},
    mutation_plane::types::{MutationId, MutatorId},
    mutator_protocol::descriptor::{owned::*, MutatorDescriptor},
};
use std::collections::{BTreeMap, HashMap};

pub type MutatorParams = BTreeMap<AttrKey, AttrVal>;

/// This is a specialized/infallible version of SyncMutatorActuator
pub trait MutatorActuator {
    fn mutator_id(&self) -> MutatorId;

    fn inject(&mut self, mutation_id: MutationId, params: BTreeMap<AttrKey, AttrVal>);

    fn reset(&mut self);
}

pub trait MutatorActuatorDescriptor: MutatorActuator + MutatorDescriptor {
    fn as_dyn(&mut self) -> &mut dyn MutatorActuatorDescriptor;
}

#[derive(Debug)]
pub struct BasicMutator {
    mutator_id: MutatorId,
    descriptor: OwnedMutatorDescriptor,
    active_mutation: Option<MutationId>,
}

impl BasicMutator {
    pub fn new(descriptor: OwnedMutatorDescriptor) -> Self {
        Self {
            mutator_id: MutatorId::allocate(),
            descriptor,
            active_mutation: None,
        }
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.active_mutation.is_some()
    }

    pub fn active_mutation(&self) -> Option<MutationId> {
        self.active_mutation
    }
}

impl MutatorActuatorDescriptor for BasicMutator {
    fn as_dyn(&mut self) -> &mut dyn MutatorActuatorDescriptor {
        self
    }
}

impl MutatorDescriptor for BasicMutator {
    fn get_description_attributes(&self) -> Box<dyn Iterator<Item = (AttrKey, AttrVal)> + '_> {
        self.descriptor.clone().into_description_attributes()
    }
}

impl MutatorActuator for BasicMutator {
    fn mutator_id(&self) -> MutatorId {
        self.mutator_id
    }

    fn inject(&mut self, mutation_id: MutationId, params: MutatorParams) {
        assert!(params.len() == 1, "BasicMutator expects 1 parameter");
        self.active_mutation = Some(mutation_id);
    }

    fn reset(&mut self) {
        self.active_mutation = None;
    }
}

pub fn failure_mutator_descriptor() -> OwnedMutatorDescriptor {
    OwnedMutatorDescriptor {
        name: "Producer message corruption".to_owned().into(),
        description: "Corrupt a message in the producer task".to_owned().into(),
        layer: MutatorLayer::Operational.into(),
        group: "system".to_owned().into(),
        operation: MutatorOperation::Corrupt.into(),
        statefulness: MutatorStatefulness::Permanent.into(),
        organization_custom_metadata: OrganizationCustomMetadata::new(
            "system".to_string(),
            HashMap::from([
                ("id".to_string(), 1_i64.into()),
                ("name".to_string(), "rv234".into()),
                ("component_name".to_string(), "power-gateway".into()),
            ]),
        ),
        params: vec![
            OwnedMutatorParamDescriptor::new(AttrType::Integer, "payload".to_owned())
                .unwrap()
                .with_description("Corrupt payload")
                .with_value_min(32)
                .with_value_max(128),
        ],
    }
}
