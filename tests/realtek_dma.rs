#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]
extern crate alloc;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::drivers::{pci, rtl8168::Rtl8168};
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    socket::dhcpv4,
    time::Instant,
    wire::EthernetAddress,
};
entry_point!(main, config = &osel::BOOTLOADER_CONFIG);
fn main(info: &'static mut BootInfo) -> ! {
    osel::avganec::init(info.framebuffer.take());
    osel::init(info);
    osel::net::wait_ready().expect("primary NIC lease before leak baseline");
    test_main();
    osel::hlt_loop();
}
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}
fn now() -> Instant {
    Instant::from_millis((osel::time::monotonic_ns() / 1_000_000) as i64)
}
#[test_case]
fn realtek_dma_reinitializes_after_traffic_and_releases_every_page() {
    let device = pci::scan()
        .into_iter()
        .rev()
        .find(Rtl8168::matches)
        .expect("secondary Realtek NIC");
    for _ in 0..3 {
        let mut nic = Rtl8168::new(device).expect("Realtek initialization");
        let mut interface = Interface::new(
            Config::new(EthernetAddress(nic.mac).into()),
            &mut nic,
            now(),
        );
        let mut sockets = SocketSet::new(alloc::vec![]);
        let dhcp = sockets.add(dhcpv4::Socket::new());
        let deadline = osel::time::monotonic_ns() + 10_000_000_000;
        loop {
            interface.poll(now(), &mut nic, &mut sockets);
            if matches!(
                sockets.get_mut::<dhcpv4::Socket>(dhcp).poll(),
                Some(dhcpv4::Event::Configured(_))
            ) {
                break;
            }
            assert!(
                osel::time::monotonic_ns() < deadline,
                "secondary Realtek DHCP timeout"
            );
            osel::time::sleep_until(osel::time::monotonic_ns() + 10_000_000).unwrap();
        }
        assert!(nic.sent >= 2 && nic.received >= 2);
        drop(sockets);
        drop(interface);
        drop(nic);
        assert_eq!(
            device.read_word(4) & 4,
            0,
            "bus mastering must stop before DMA pages are freed"
        );
    }
}
