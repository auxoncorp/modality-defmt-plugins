# Test system for modality-defmt-plugins

This project serves as an integration test suite for the plugins.
It's a simple RTICv1 based application running on an atsamd51g19a emulated in [Renode](https://renode.readthedocs.io/en/latest/).

See the integration test [workflow](../.github/workflows/integration_tests.yml) for more details on how we use it
in this repository.

## Using the data from CI

The Integration Tests workflow produces a modality data artifact that can be downloaded for local use.

1. [Install Modality](https://docs.auxon.io/modality/installation/)
2. Navigate to the latest Integration Tests workflow run in the Actions tab
3. Download the tarball artifact (`defmt_test_system_modality_data_$DATETIME.tar.gz`)
4. Extract it (assuming in `/tmp`)
5. Run `modalityd` with the extracted data-dir
  ```bash
  modalityd --license-key '<YOU_LICENSE_KEY> --data-dir /tmp/modalityd_data
  ```
6. Setup the user and workspace
  ```bash
  modality user auth-token --use $(< /tmp/modalityd_data/user_auth_token)
  modality workspace use ci-tests
  ```

## Running Locally

To run the setup locally, first install the dependencies:
* Modality: https://docs.auxon.io/modality/installation/
* Rust (with target `thumbv7em-none-eabihf`): https://www.rust-lang.org/tools/install
* Renode: https://renode.readthedocs.io/en/latest/introduction/installing.html
* renode-run: `cargo install renode-run`
* flip-link: `cargo install flip-link`
* Install the `defmt` plugins from this repository if not using the ones provided by the `modality-reflector` package

Now you run the system with `cargo run --release`.

Once it completes, you can import the `defmt` data file `/tmp/rtt_log.bin` using the importer plugin.
