#![no_std]
#![no_main]

// A kernel stack overflow with the real IDT: the double fault handler runs on its own
// stack and the panic screen shows up

osel::panic_test!("double_fault::stack_overflow_is_shown", ["EXCEPTION: DOUBLE FAULT"], {
    stack_overflow();
});

#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    // prevent tail recursion optimizations
    unsafe { core::ptr::read_volatile(&0) };
}
