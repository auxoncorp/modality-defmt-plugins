*** Settings ***
Documentation   Integration test suite
Default Tags    test_system
Library         Process
Library         robot_framework/ModalityClientLibrary.py

# These are the defaults provided by renode, automatically set if not supplied
Suite Setup     Test System Suite Setup
Suite Teardown  Test System Suite Teardown
Test Setup      Test System Test Setup
Test Teardown   Test System Test Teardown
Resource        ${RENODEKEYWORDS}

*** Variables ***
${MUTATOR_SERVER}               ${CURDIR}/tools/mutator-server/target/x86_64-unknown-linux-gnu/release/mutator-server
${MUTATOR_SERVER_PORT}          9785
${RUN_ID_SCRIPT}                ${CURDIR}/scripts/get_test_run_id.sh
${RESC}                         ${CURDIR}/renode/robot_frameworkd_setup.resc
${FW_ELF}                       ${CURDIR}/target/thumbv7em-none-eabihf/release/atsamd-rtic-firmware
${RTT_LOG}                      /tmp/rtt_log.bin
${REFLECTOR_CONFIG}             ${CURDIR}/config/reflector-config.toml
${SPEC_NAME}                    tests

*** Keywords ***
Test System Suite Setup
    On Suite Setup                  ${SUITE_NAME}
    Setup
    Build System

Test System Suite Teardown
    On Suite Teardown
    Terminate All Processes
    Teardown

Test System Test Setup
    ${runid}                        Run Command         ${RUN_ID_SCRIPT} "${TEST NAME}"
    Set Test Variable               ${SEGMENT}          ${TEST NAME}::${runid.stdout}
    Set environment variable        MODALITY_RUN_ID     ${SEGMENT}
    On Test Setup                   ${TEST_NAME}
    Setup
    Test Setup
    Prepare Machine

Test System Test Teardown
    Run Keyword If Test Failed
      ...   On Test Failed          ${TEST_NAME}
    Run Keyword If Test Passed
      ...   On Test Passed          ${TEST_NAME}
    On Test Teardown                ${TEST_NAME}
    Test Teardown

Run Command
    [Arguments]                     ${cmd_and_args}
    ${result} =                     Run Process         ${cmd_and_args}  shell=true
    IF                              ${result.rc} != 0
        Log To Console              Error running command: ${cmd_and_args}    console=yes
        Log To Console              ${result.stdout}    console=yes
        Log To Console              ${result.stderr}    console=yes
    END
    Should Be Equal As Integers     ${result.rc}        0
    RETURN                          ${result}

Build System
    Build Firmware
    Build Mutator Server

Build Firmware
    Run Command                     cargo build --release

Build Mutator Server
    Run Command                     cd tools/mutator-server && cargo build --release

Prepare Machine
    Execute Command                 path add @${CURDIR}
    Execute Script                  ${RESC}

Start Mutator Server
    ${process}                      Start Process       ${MUTATOR_SERVER}  alias=mutator-server
    Sleep                           1s

Create Mutation
    Run Command                     deviant mutation create
    ${result}                       Run Command         netcat -W 1 127.0.0.1 ${MUTATOR_SERVER_PORT}
    ${deviant_ids}                  Evaluate            json.loads("""${result.stdout}""")    json
    Set Test Variable               ${MUTATOR_ID}       ${deviant_ids['mutator_id']}
    Set Test Variable               ${MUTATION_ID}      ${deviant_ids['mutation_id']}

Import Data
    Run Command                     modality-reflector import --config ${REFLECTOR_CONFIG} defmt --elf-file ${FW_ELF} ${RTT_LOG}
    Run Command                     modality workspace sync-indices

Evaluate Specs
    Run Command                     conform spec eval --name ${SPEC_NAME} --segment '${SEGMENT}'

*** Test Cases ***
Nominal System Execution
    [Documentation]                 Boots the system and runs for a period of time
    [Tags]                          firmware

    ${startup_nonce}                On Start Component          embedded
    Execute Command                 write_startup_nonce ${startup_nonce}
    Execute Command                 emulation RunFor "00:00:15"
    Import Data
    Evaluate Specs

Mutated System Execution
    [Documentation]                 Boots the system with a mutation and runs for a period of time
    [Tags]                          firmware deviant mutation

    Start Mutator Server
    Create Mutation
    ${startup_nonce}                On Start Component          embedded
    Execute Command                 write_startup_nonce ${startup_nonce}
    Execute Command                 write_staged_mutation "${MUTATOR_ID}" "${MUTATION_ID}"
    Execute Command                 emulation RunFor "00:00:15"
    Import Data
    Evaluate Specs
