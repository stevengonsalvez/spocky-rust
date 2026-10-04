//! The comparison boundaries of `shed_if_needed` (`capacity.ex:437-471`), with exact memory
//! readings. The pinned relay reads `:erlang.memory(:total)` itself, so a differential cannot
//! land a reading on the watermark or the recovery level; these cases follow the source.

use spocky_relay::capacity::{Capacity, Config, Effect, Pid};
use std::panic::{AssertUnwindSafe, catch_unwind};

const STEP: i64 = 33_554_418;

fn capacity(watermark: i64) -> Capacity {
    Capacity::new(Config {
        ingress_budget_bytes: 1_000_000,
        ingress_weight: 1,
        memory_watermark_bytes: watermark,
        native_units_per_second: 1_000_000_000,
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

fn attach_many(capacity: &mut Capacity, count: u64) {
    for pid in 1..=count {
        let token = capacity
            .admit_connection("a", 1_000, Pid(pid), true)
            .expect("admitted");
        capacity
            .attach_connection(token, Pid(pid))
            .expect("attached");
    }
}

fn pressure_victims(capacity: &mut Capacity) -> Vec<Pid> {
    capacity
        .take_effects()
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::MemoryPressure(pid) => Some(pid),
            _ => None,
        })
        .collect()
}

#[test]
fn more_candidates_than_the_first_batch_cap_wait_for_the_next_pass() {
    let mut capacity = capacity(1_000_000_000);
    attach_many(&mut capacity, 200);
    capacity.take_effects();
    // The excess asks for 100 victims; the first batch is capped at 64, newest first.
    capacity.check_now(1_000_000_000 + 99 * STEP + 1);
    let first = pressure_victims(&mut capacity);
    assert_eq!(first.len(), 64);
    assert_eq!(first[0], Pid(200));
    assert_eq!(first[63], Pid(137));
    let pressure = capacity.pressure().unwrap();
    assert_eq!((pressure.victims, pressure.batch), (64, 64));
    // No relief since the last pass: the batch doubles to 128 and takes the next 128.
    capacity.pressure_recheck(1_000_000_000 + 99 * STEP + 1);
    let second = pressure_victims(&mut capacity);
    assert_eq!(second.len(), 128);
    assert_eq!(second[0], Pid(136));
    assert_eq!(capacity.pressure().unwrap().batch, 128);
    // Eight sockets remain: the third pass sheds them and runs out of candidates.
    capacity.pressure_recheck(1_000_000_000 + 99 * STEP + 1);
    assert_eq!(pressure_victims(&mut capacity).len(), 8);
    assert_eq!(capacity.pressure().unwrap().victims, 8);
}

#[test]
fn the_delivery_wait_floors_the_native_difference_once() {
    // (native units per second, elapsed native units, microseconds): the unit is what
    // `System.convert_time_unit(1, :second, :native)` reports, nanoseconds on the pinned runtime.
    for (unit, elapsed, microseconds) in [
        (1_000_000_000, 0, 0),
        (1_000_000_000, 999, 0),
        (1_000_000_000, 1_000, 1),
        (1_000_000_000, 1_999, 1),
        (1_000_000_000, 2_000_000_001, 2_000_000),
        (1_000_000, 3, 3),
        (3, 2, 666_666),
    ] {
        let mut capacity = Capacity::new(Config {
            ingress_budget_bytes: 1_000_000,
            ingress_weight: 1,
            memory_watermark_bytes: 0,
            native_units_per_second: unit,
        });
        let token = capacity.admit_connection("a", 1, Pid(1), true).unwrap();
        capacity.attach_connection(token, Pid(1)).unwrap();
        let message = capacity.admit_message(Pid(1), 10).expect("admitted");
        capacity.take_effects();
        capacity
            .start_delivery(message, Pid(1), 5_000)
            .expect("started");
        capacity.finish_message(message, 5_000 + elapsed);
        assert!(
            capacity
                .take_effects()
                .contains(&Effect::ObserveDeliveryWait { microseconds }),
            "unit {unit} elapsed {elapsed}"
        );
    }
}

/// A holder that attaches twice leaves a stale key in the `active` tree (`capacity.ex:121-146`);
/// the next shed crashes the relay's Capacity process after it already sent
/// `:relay_memory_pressure` to the earlier victims. The runner contract: catch the unwind, drain
/// `take_effects`, reset with `Capacity::new`.
#[test]
fn a_stale_active_key_panics_after_the_earlier_victims_were_reported() {
    let mut capacity = capacity(1_000_000_000);
    let first = capacity.admit_connection("a", 9, Pid(1), true).unwrap();
    capacity.attach_connection(first, Pid(1)).unwrap();
    let second = capacity.admit_connection("a", 9, Pid(1), true).unwrap();
    capacity.attach_connection(second, Pid(1)).unwrap();
    let other = capacity.admit_connection("a", 9, Pid(2), true).unwrap();
    capacity.attach_connection(other, Pid(2)).unwrap();
    capacity.release_connection(second);
    capacity.take_effects();
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        capacity.check_now(1_000_000_000 + STEP + 1);
    }));
    assert!(crashed.is_err());
    // The victim before the stale key was reported, with its metric.
    assert_eq!(
        capacity.take_effects(),
        vec![
            Effect::MemoryPressure(Pid(2)),
            Effect::MemoryPressureDisconnect
        ]
    );
    // The crashed Capacity held state (two sockets, three connections); the runner's reset leaves
    // none of it.
    assert_ne!(capacity.sizes(), [0; 5]);
    let reset = Capacity::new(Config {
        ingress_budget_bytes: 1_000_000,
        ingress_weight: 1,
        memory_watermark_bytes: 1_000_000_000,
        native_units_per_second: 1_000_000_000,
    });
    assert_eq!(reset.sizes(), [0; 5]);
    assert!(reset.pressure().is_none());
}
