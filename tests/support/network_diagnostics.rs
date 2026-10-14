#[test_case]
fn ping_gateway_timeout_cancel_and_cleanup() {
    use core::fmt::Write;
    use osel_core::errno::Errno;
    let mut out = osel_core::exception::FixedBuf::<4096>::new();
    for _ in 0..3 {
        assert_eq!(osel::net::ping("gateway", 1, &mut out), Ok(0));
    }
    assert!(out.as_str().contains("received, 0% loss"));
    assert!(out.as_str().contains("rtt min/avg/max"));
    assert_eq!(osel::net::ping("10.0.2.250", 1, &mut out), Ok(1));
    assert!(out.as_str().contains("timeout from 10.0.2.250"));
    let mut calls = 0;
    assert_eq!(
        osel::net::ping_with_cancel("gateway", 10, &mut out, || {
            calls += 1;
            calls > 1
        }),
        Ok(1)
    );
    for target in [
        "0.0.0.0",
        "255.255.255.255",
        "10.0.2.255",
        "224.0.0.1",
        "127.0.0.1",
        "bad-ip",
    ] {
        assert_eq!(osel::net::ping(target, 1, &mut out), Err(Errno::EINVAL));
    }
    assert_eq!(osel::net::ping("gateway", 0, &mut out), Err(Errno::EINVAL));
    assert_eq!(osel::net::ping("gateway", 11, &mut out), Err(Errno::EINVAL));
    for section in ["ip", "route", "dns", "tcp"] {
        osel::net::diagnostics(section, &mut out).unwrap();
    }
    assert!(out.as_str().contains("default via 10.0.2.2"));
    assert!(out.as_str().contains("DNS 10.0.2.3"));
    for _ in 0..3 {
        assert_eq!(osel::net::lookup("a..b"), Err(Errno::EINVAL));
    }
    let endpoint = osel::fs::read_file("/bin/network-test-address").unwrap();
    let text = core::str::from_utf8(&endpoint).unwrap();
    let (address, port) = text.trim().split_once(':').unwrap();
    let port = port.parse().unwrap();
    for _ in 0..3 {
        osel::net::tcp_check(address, port).expect("local Tetris server TCP port");
    }
    assert_eq!(
        osel::net::tcp_check("10.0.2.2", 1),
        Err(Errno::ECONNREFUSED)
    );
    assert_eq!(
        osel::net::tcp_check("10.0.2.250", 8080),
        Err(Errno::ETIMEDOUT)
    );
    let _ = writeln!(out, "done");
}
