//! The comparison boundaries of `shed_if_needed` (`capacity.ex:437-471`), with exact memory
//! readings. The pinned relay reads `:erlang.memory(:total)` itself, so a differential cannot
//! land a reading on the watermark or the recovery level; these cases follow the source.

use spocky_relay::capacity::{Capacity, Config, Pid};

const STEP: i64 = 33_554_418;

fn capacity(watermark: i64) -> Capacity {
    Capacity::new(Config {
        ingress_budget_bytes: 1_000_000,
        ingress_weight: 1,
        memory_watermark_bytes: watermark,
    })
}

fn with_socket(watermark: i64) -> Capacity {
    let mut capacity = capacity(watermark);
    let token = capacity
        .admit_connection("a", 1, Pid(1), true)
        .expect("admitted");
    capacity.attach_connection(token, Pid(1)).expect("attached");
    capacity
}

#[test]
fn a_reading_equal_to_the_watermark_is_pressure() {
    let mut below = with_socket(1_000_000_000);
    below.check_now(999_999_999);
    assert!(below.pressure().is_none());
    let mut equal = with_socket(1_000_000_000);
    equal.check_now(1_000_000_000);
    let pressure = equal.pressure().expect("pressure");
    assert_eq!((pressure.victims, pressure.batch), (1, 1));
}

#[test]
fn the_first_batch_rounds_up_by_whole_steps_and_caps_at_64() {
    for (excess, batch) in [
        (0, 1),
        (1, 1),
        (STEP, 1),
        (STEP + 1, 2),
        (63 * STEP, 63),
        (63 * STEP + 1, 64),
        (500 * STEP, 64),
    ] {
        let mut capacity = with_socket(1_000_000_000);
        capacity.check_now(1_000_000_000 + excess);
        assert_eq!(capacity.pressure().unwrap().batch, batch, "excess {excess}");
    }
}

#[test]
fn a_reading_equal_to_the_recovery_level_clears_the_pressure() {
    let watermark = 1_000_000_000;
    let recovery = watermark - STEP;
    let mut capacity = with_socket(watermark);
    capacity.check_now(watermark);
    assert!(capacity.pressure().is_some());
    capacity.check_now(recovery + 1);
    assert!(capacity.pressure().is_some());
    capacity.check_now(recovery);
    assert!(capacity.pressure().is_none());
}
