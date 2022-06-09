// $ DEFMT_LOG=info cargo rb exti
#![no_main]
#![no_std]

use core::sync::atomic::{AtomicUsize, Ordering};

use defmt_rtt as _; // global logger
use panic_probe as _;
use stm32f4xx_hal as _; // memory layout

// same panicking *behavior* as `panic-probe` but doesn't print a panic message
// this prevents the panic message being printed *twice* when `defmt::panic` is invoked
#[defmt::panic_handler]
fn panic() -> ! {
    cortex_m::asm::udf()
}

static COUNT: AtomicUsize = AtomicUsize::new(0);
defmt::timestamp!("{=usize}", {
    // NOTE(no-CAS) `timestamps` runs with interrupts disabled
    let n = COUNT.load(Ordering::Relaxed);
    COUNT.store(n + 1, Ordering::Relaxed);
    n
});

/// Terminates the application and makes `probe-run` exit with exit-code = 0
pub fn exit() -> ! {
    loop {
        cortex_m::asm::bkpt();
    }
}

#[rtic::app(device = stm32f4xx_hal::pac, dispatchers = [USART1])]
mod app {
    use stm32f4xx_hal::{
        delay::{self, Delay},
        gpio::{self, gpioa::PA0, gpioc::PC13, Edge, ExtiPin, Input, OpenDrain, Output, PullUp},
        pac::{self, SPI2},
        prelude::*,
        spi::{self, Mode, Phase, Polarity, Spi},
        timer::{monotonic::MonoTimer, Timer},
    };
    use sx127x_lora::LoRa;

    type LoRaDriver = LoRa<
        spi::Spi<
            SPI2,
            (
                gpio::Pin<gpio::Alternate<gpio::PushPull, 5>, 'B', 13>,
                gpio::Pin<gpio::Alternate<gpio::PushPull, 5>, 'B', 14>,
                gpio::Pin<gpio::Alternate<gpio::PushPull, 5>, 'B', 15>,
            ),
            spi::TransferModeBidi,
        >,
        gpio::Pin<gpio::Output<gpio::PushPull>, 'B', 12>,
        gpio::Pin<gpio::Output<gpio::PushPull>, 'A', 8>,
        delay::Delay,
    >;

    #[monotonic(binds = TIM5, default = true)]
    type Tonic = MonoTimer<pac::TIM5, 48_000_000>;

    #[shared]
    struct Shared {
        led: PC13<Output<OpenDrain>>,
    }

    #[local]
    struct Local {
        btn: PA0<Input<PullUp>>,
        lora: LoRaDriver,
    }

    #[init]
    fn init(mut ctx: init::Context) -> (Shared, Local, init::Monotonics) {
        // Set up the system clock.
        let rcc = ctx.device.RCC.constrain();
        let clocks = rcc.cfgr.sysclk(48.mhz()).freeze();

        // Set up the LED.
        let gpioc = ctx.device.GPIOC.split();
        let led = gpioc.pc13.into_open_drain_output();

        // Set up the button.
        let gpioa = ctx.device.GPIOA.split();
        let mut btn = gpioa.pa0.into_pull_up_input();
        let mut sys_cfg = ctx.device.SYSCFG.constrain();
        btn.make_interrupt_source(&mut sys_cfg);
        btn.enable_interrupt(&mut ctx.device.EXTI);
        btn.trigger_on_edge(&mut ctx.device.EXTI, Edge::Falling);

        let delay = Delay::new(ctx.core.SYST, &clocks);

        let rst = gpioa.pa8.into_push_pull_output();

        let gpiob = ctx.device.GPIOB.split();

        let sck = gpiob.pb13.into_alternate();
        let miso = gpiob.pb14.into_alternate();
        let mosi = gpiob.pb15.into_alternate();
        let cs = gpiob.pb12.into_push_pull_output();

        let mode = Mode {
            polarity: Polarity::IdleLow,
            phase: Phase::CaptureOnFirstTransition,
        };

        // Change spi transfer mode to Bidi for more efficient operations.
        let spi = Spi::new(
            ctx.device.SPI2,
            (sck, miso, mosi),
            mode,
            20_000.hz(),
            &clocks,
        )
        .to_bidi_transfer_mode();

        let lora = sx127x_lora::LoRa::new(spi, cs, rst, 433, delay)
            .expect("Failed to communicate with radio module!");

        let mono = Timer::new(ctx.device.TIM5, &clocks).monotonic();

        defmt::info!("Press button!");
        send::spawn_after(1.secs()).ok();
        (Shared { led }, Local { btn, lora }, init::Monotonics(mono))
    }

    #[idle]
    fn idle(_: idle::Context) -> ! {
        loop {
            continue;
        }
    }

    #[task(binds = EXTI0, local = [btn], shared = [led])]
    fn on_exti(mut ctx: on_exti::Context) {
        ctx.local.btn.clear_interrupt_pending_bit();
        ctx.shared.led.lock(|l| l.toggle());
        defmt::warn!("Button was pressed!");
    }

    #[task(shared = [led], local = [lora])]
    fn send(mut ctx: send::Context) {
        ctx.shared.led.lock(|l| l.toggle());
        defmt::info!("Send!");
        ctx.local
            .lora
            .set_tx_power(17, 1)
            .expect("Unable to set TX power"); //Using PA_BOOST. See your board for correct pin.

        let message = "Hello, world!";
        let mut buffer = [0; 255];
        for (i, c) in message.chars().enumerate() {
            buffer[i] = c as u8;
        }

        ctx.local
            .lora
            .transmit_payload(buffer, message.len())
            .map(|e| defmt::info!("{:?}", e))
            .unwrap();

        send::spawn_after(1.secs()).ok();
    }
}
