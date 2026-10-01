//! The shared test helper that keeps tests off the production daemon ports.

mod common;

use common::assert_disposable_listen;

#[test]
fn production_ports_are_refused_in_every_listen_form() {
    for listen in ["127.0.0.1:6767", "127.0.0.1:6768", "6767", "[::1]:6768"] {
        assert!(
            std::panic::catch_unwind(|| assert_disposable_listen(listen)).is_err(),
            "{listen}"
        );
    }
    for listen in ["127.0.0.1:0", "127.0.0.1:41000", "/tmp/spocky.sock", "0"] {
        assert_disposable_listen(listen);
    }
}
