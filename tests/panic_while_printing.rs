#![no_std]
#![no_main]

// A panic while the screen and serial locks are held still shows the panic screen

fn hold_locks_and_panic() -> ! {
    let _screen = osel::avganec::WRITER.lock();
    let _serial = osel::cereal::SERIAL1.lock();
    panic!("panic while holding the print locks");
}

osel::panic_test!("panic_while_printing::screen_still_works", ["panic while holding the print locks"], {
    hold_locks_and_panic();
});
