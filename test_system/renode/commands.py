from Antmicro import Renode
import uuid

def mc_clear_modality_noint_vars():
    sysbus = self.Machine["sysbus"]
    var_startup_nonce_addr = sysbus.GetSymbolAddress("MODALITY_TEST_FRAMEWORK_NONCE")
    sysbus.WriteDoubleWord(var_startup_nonce_addr, 0)

def mc_write_startup_nonce(nonce):
    print("Writing startup nonce = %d" % nonce)
    sysbus = self.Machine["sysbus"]
    var_startup_nonce_addr = sysbus.GetSymbolAddress("MODALITY_TEST_FRAMEWORK_NONCE")
    sysbus.WriteDoubleWord(var_startup_nonce_addr, nonce)

def mc_clear_deviant_noint_vars():
    sysbus = self.Machine["sysbus"]
    var_mutation_staged_addr = sysbus.GetSymbolAddress("DEVIANT_MUTATION_STAGED")
    sysbus.WriteDoubleWord(var_mutation_staged_addr, 0)

def mc_write_staged_mutation(mutator_uuid_str, mutation_uuid_str):
    print("Writing staged mutation, mutator_id = %s, mutation_id = %s" % (mutator_uuid_str, mutation_uuid_str))
    mutator_uuid = uuid.UUID(mutator_uuid_str)
    mutation_uuid = uuid.UUID(mutation_uuid_str)

    sysbus = self.Machine["sysbus"]
    var_mutation_staged_addr = sysbus.GetSymbolAddress("DEVIANT_MUTATION_STAGED")
    var_mutator_id_addr = sysbus.GetSymbolAddress("DEVIANT_MUTATOR_ID");
    var_mutation_id_addr = sysbus.GetSymbolAddress("DEVIANT_MUTATION_ID");

    for offset, b in enumerate(mutator_uuid.bytes):
        val = int(b.encode('hex'), 16)
        sysbus.WriteByte(var_mutator_id_addr + offset, val)
    for offset, b in enumerate(mutation_uuid.bytes):
        val = int(b.encode('hex'), 16)
        sysbus.WriteByte(var_mutation_id_addr + offset, val)

    sysbus.WriteDoubleWord(var_mutation_staged_addr, 1)

def mc_wait_for_done():
    while not emulationManager.CurrentEmulation.TryGetFromBag[str]('status')[0]:
        time.sleep(0.1)
    monitor.Parse('q')
