#![no_main]
#![no_std]

use defmt_rtt as _;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[rtic::app(device = atsamd_hal::pac, peripherals = true, dispatchers = [FREQM])]
mod app {
    use atsamd_hal::{
        clock::v2 as clock,
        gpio::{AlternateD, Pin, Pins, PA04, PA05, PA16, PA17},
        prelude::*,
        sercom::{
            uart::{self, BaudMode, Flags, Oversampling},
            IoSet3, Sercom0, Sercom3,
        },
    };
    use core::mem::MaybeUninit;
    use defmt::{debug, info, trace, warn};
    use dwt_systick_monotonic::{DwtSystick, ExtU64};

    type Uart0Pads = uart::Pads<Sercom0, IoSet3, Pin<PA05, AlternateD>, Pin<PA04, AlternateD>>;
    type Uart0 = uart::Uart<uart::Config<Uart0Pads>, uart::Duplex>;
    type Uart3Pads = uart::Pads<Sercom3, IoSet3, Pin<PA16, AlternateD>, Pin<PA17, AlternateD>>;
    type Uart3 = uart::Uart<uart::Config<Uart3Pads>, uart::Duplex>;

    const SYSFREQ: u32 = 1_000_000;
    #[monotonic(binds = SysTick, default = true)]
    type Mono = DwtSystick<SYSFREQ>;

    #[shared]
    struct Shared {}

    #[local]
    struct Local {
        uart0: Uart0,
        uart3: Uart3,
        staged_mutation: Option<(DeviantUuid, DeviantUuid)>,
    }

    /// Mutator/Mutation UUID byte array
    type DeviantUuid = [u8; 16];

    #[link_section = ".uninit.modality0"]
    #[no_mangle]
    static mut MODALITY_TEST_FRAMEWORK_NONCE: MaybeUninit<u32> = MaybeUninit::uninit();

    /// Deviant staged-mutation related.
    /// These get initialized by renode on startup based on whether
    /// or not a mutation is staged.
    #[link_section = ".uninit.deviant0"]
    #[no_mangle]
    static mut DEVIANT_MUTATION_STAGED: MaybeUninit<u32> = MaybeUninit::uninit();

    #[link_section = ".uninit.deviant1"]
    #[no_mangle]
    static mut DEVIANT_MUTATOR_ID: MaybeUninit<DeviantUuid> = MaybeUninit::uninit();

    #[link_section = ".uninit.deviant2"]
    #[no_mangle]
    static mut DEVIANT_MUTATION_ID: MaybeUninit<DeviantUuid> = MaybeUninit::uninit();

    #[init]
    fn init(mut ctx: init::Context) -> (Shared, Local, init::Monotonics) {
        defmt::timestamp!("{=u64:us}", monotonics::now().ticks());

        let mut device = ctx.device;

        let (_buses, clocks, _tokens) = clock::clock_system_at_reset(
            device.OSCCTRL,
            device.OSC32KCTRL,
            device.GCLK,
            device.MCLK,
            &mut device.NVMCTRL,
        );

        let pins = Pins::new(device.PORT);

        let (_, _, _, mclk) = unsafe { clocks.pac.steal() };

        let mono: Mono = DwtSystick::new(&mut ctx.core.DCB, ctx.core.DWT, ctx.core.SYST, SYSFREQ);

        let baud = 115200.Hz();
        let uart_rx = pins.pa05;
        let uart_tx = pins.pa04;
        let pads = uart::Pads::default().rx(uart_rx).tx(uart_tx);
        let mut uart0 = uart::Config::new(&mclk, device.SERCOM0, pads, clocks.gclk0.freq())
            .baud(baud, BaudMode::Fractional(Oversampling::Bits16))
            .enable();
        uart0.enable_interrupts(Flags::RXC);
        let uart_rx = pins.pa16;
        let uart_tx = pins.pa17;
        let pads = uart::Pads::<Sercom3, IoSet3>::default()
            .rx(uart_rx)
            .tx(uart_tx);
        let uart3 = uart::Config::new(&mclk, device.SERCOM3, pads, clocks.gclk0.freq())
            .baud(baud, BaudMode::Fractional(Oversampling::Bits16))
            .enable();

        let test_framework_nonce = unsafe { MODALITY_TEST_FRAMEWORK_NONCE.assume_init() };
        if test_framework_nonce != 0 {
            debug!("test_framework_ack::nonce={=u32}", test_framework_nonce);
        }

        info!("Initializing app");
        debug!(
            "crate_info::pkg_name={=str},pkg_version={=str},profile={=str}",
            crate::built_info::PKG_NAME,
            crate::built_info::PKG_VERSION,
            crate::built_info::PROFILE
        );
        debug!(
            "build_info::date={=str},rustc={=str},git_commit={=str}",
            crate::built_info::BUILT_TIME_UTC,
            crate::built_info::RUSTC_VERSION,
            crate::built_info::GIT_COMMIT_HASH.unwrap_or("NA"),
        );

        // Handle staged deviant mutation, if one exists
        let mutation_staged = unsafe { DEVIANT_MUTATION_STAGED.assume_init() };
        let staged_mutation = if mutation_staged != 0 {
            let mutator_id = unsafe { DEVIANT_MUTATOR_ID.assume_init() };
            let mutation_id = unsafe { DEVIANT_MUTATION_ID.assume_init() };

            // Write down announcement and communication in the log
            trace!(
                "modality.mutator.announced::mutator.id={=[u8; 16]}",
                mutator_id
            );
            trace!("modality.mutation.command_communicated::mutation.success={=bool},mutator.id={=[u8; 16]},mutation.id={=[u8; 16]}",
                true,
                mutator_id,
                mutation_id);
            Some((mutator_id, mutation_id))
        } else {
            None
        };

        blinky::spawn().unwrap();

        (
            Shared {},
            Local {
                uart0,
                uart3,
                staged_mutation,
            },
            init::Monotonics(mono),
        )
    }

    #[idle]
    fn idle(_ctx: idle::Context) -> ! {
        info!("Starting idle task");
        loop {
            cortex_m::asm::wfi();
        }
    }

    #[task(local = [uart3, var: u32 = 0])]
    fn blinky(ctx: blinky::Context) {
        let uart = ctx.local.uart3;
        *ctx.local.var += 1;
        info!("blink::constant_key=1,var={=u32}", ctx.local.var);

        // Send some data to Sercom0 to fire the hw task.
        // We've got Sercom0 connected to Sercom3 in renode
        uart.write(*ctx.local.var as u8).unwrap();

        blinky::spawn_after(1_u64.secs()).unwrap();
    }

    #[task(binds = SERCOM0_2, local = [uart0], priority = 2)]
    fn uart_handler(ctx: uart_handler::Context) {
        let uart = ctx.local.uart0;
        let data = uart.read().map(|d| d as u16).unwrap_or(0xFFFF);
        warn!("uart_rx::data={=u16}", data);
        producer::spawn().ok();
    }

    #[derive(Debug, defmt::Format)]
    struct IpcMessage {
        data: u16,
    }

    #[task(local = [data: u16 = 0, staged_mutation])]
    fn producer(ctx: producer::Context) {
        *ctx.local.data += 1;
        let mut msg = IpcMessage {
            data: *ctx.local.data,
        };
        if let Some((mutator_id, mutation_id)) = ctx.local.staged_mutation.take() {
            // Write down the injection in the log
            trace!("modality.mutation.injected::mutation.success={=bool},mutator.id={=[u8; 16]},mutation.id={=[u8; 16]}",
                true,
                mutator_id,
                mutation_id);
            msg.data = u16::MAX;
        }
        info!("send_data::data={=u16}", msg.data);
        consumer::spawn(msg).ok();
    }

    #[task(capacity = 2)]
    fn consumer(_ctx: consumer::Context, msg: IpcMessage) {
        info!("recv_data::data={=u16}", msg.data);

        match msg.data {
            u16::MAX => panic!("Message corruption!"),
            6 => panic!("data == 6"),
            _ => (),
        }
    }
}

mod panic_impl {
    use core::panic::PanicInfo;
    use core::sync::atomic::AtomicBool;
    use core::sync::atomic::{self, Ordering};

    #[panic_handler]
    fn panic(info: &PanicInfo) -> ! {
        static PANICKED: AtomicBool = AtomicBool::new(false);

        cortex_m::interrupt::disable();

        if !PANICKED.load(Ordering::Relaxed) {
            PANICKED.store(true, Ordering::Relaxed);

            defmt::error!("panic::msg={}", defmt::Display2Format(info));
        }

        wait_forever();
    }

    // NOTE: we use this symbol in renode to indicate it's time to shutdown
    #[inline(never)]
    #[no_mangle]
    fn wait_forever() -> ! {
        loop {
            atomic::compiler_fence(Ordering::SeqCst);
        }
    }
}
