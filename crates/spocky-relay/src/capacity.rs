//! `PaseoRelay.Capacity`: connection admission, ingress budget, backpressure bookkeeping and
//! memory-pressure shedding.
//!
//! The BEAM process is a `GenServer`; this is the same state machine with its inputs made
//! explicit. Calls pass the caller and the liveness the BEAM reads from the scheduler, process
//! exits arrive through [`Capacity::process_down`], timer firings through
//! [`Capacity::expire`], [`Capacity::check`] and [`Capacity::pressure_recheck`], and the memory
//! reading (`:erlang.memory(:total)`) is an argument. What the process does to the world is
//! queued as [`Effect`]s and drained with [`Capacity::take_effects`].

use spocky_relay_protocol::limits::MAXIMUM_MESSAGE_PAYLOAD_BYTES;
use std::collections::{BTreeMap, BTreeSet};

/// An Erlang process identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Pid(pub u64);

/// A reference the process hands out: a connection, reservation or message token.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Token(pub u64);

/// A process monitor reference.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Monitor(pub u64);

/// `@reservation_timeout_ms`.
pub const RESERVATION_TIMEOUT_MS: u64 = 5_000;
/// `@check_interval_ms`.
pub const CHECK_INTERVAL_MS: u64 = 1_000;
/// `@pressure_recheck_ms`.
pub const PRESSURE_RECHECK_MS: u64 = 100;
const INITIAL_MAX_SHED_BATCH: i128 = 64;
const MAX_SHED_BATCH: i128 = 1_024;

/// The part of `PaseoRelay.Config` the process reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    pub ingress_budget_bytes: i64,
    pub ingress_weight: i64,
    pub memory_watermark_bytes: i64,
}

/// What the BEAM process does besides changing its state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// `Process.send_after(self(), {:expire, token}, 5_000)`.
    StartReservationTimer(Token),
    /// `Process.cancel_timer(timer)` of a reservation.
    CancelReservationTimer(Token),
    /// `Process.monitor(holder)`.
    Monitor { monitor: Monitor, pid: Pid },
    /// `Process.demonitor(monitor, [:flush])`.
    Demonitor(Monitor),
    /// `Process.send_after(self(), :check, 1_000)`.
    ScheduleCheck,
    /// `Process.send_after(self(), :pressure_recheck, 100)`.
    SchedulePressureRecheck,
    /// `send(socket, :relay_memory_pressure)`.
    MemoryPressure(Pid),
    /// `PaseoRelay.Metrics.inc(:memory_pressure_disconnects)`.
    MemoryPressureDisconnect,
    /// `PaseoRelay.Metrics.observe_delivery_wait(native_duration)`, in microseconds.
    ObserveDeliveryWait { microseconds: i64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmitConnectionError {
    Unavailable,
    Pressure,
    ConfigurationMismatch,
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmitMessageError {
    Closed,
    Pressure,
    MessageExceedsBudget,
    BudgetExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartDeliveryError {
    Closed,
    Pressure,
    Expired,
}

/// `{:error, :expired}` of `attach_connection/2`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Expired;

/// The `gauges/1` map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gauges {
    pub active_websockets: i64,
    pub ingress_reserved_bytes: i64,
    pub inflight_delivery_bytes: i64,
    pub backpressured_sources: i64,
}

/// `admission_state/3`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    Pressure,
    ConfigurationMismatch,
    ConnectionCapacity,
    Open,
}

/// The `status/2` reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Status {
    pub admission: Admission,
    pub gauges: Gauges,
}

/// The `pressure` map of the state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pressure {
    pub memory: i64,
    pub victims: i64,
    pub batch: i64,
}

#[derive(Clone, Debug)]
struct NamespaceState {
    limit: i64,
    active: i64,
}

#[derive(Clone, Copy, Debug)]
enum ConnectionStatus {
    Reservation,
    Active(Pid),
}

#[derive(Clone, Debug)]
struct Connection {
    namespace: String,
    holder: Pid,
    monitor: Monitor,
    status: ConnectionStatus,
}

#[derive(Clone, Debug)]
struct SocketState {
    monitor: Monitor,
    connection: Token,
    messages: BTreeSet<Token>,
    active_key: Option<u64>,
    blocked_key: Option<u64>,
    shedding: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageStatus {
    Reserved,
    Delivering,
}

#[derive(Clone, Debug)]
struct Message {
    socket: Pid,
    payload_bytes: i64,
    weighted_bytes: i64,
    status: MessageStatus,
    /// Microseconds on the caller's monotonic clock.
    started: Option<i64>,
}

#[derive(Debug)]
pub struct Capacity {
    ingress_limit: i64,
    ingress_weight: i64,
    watermark: i64,
    namespaces: BTreeMap<String, NamespaceState>,
    connections: BTreeMap<Token, Connection>,
    sockets: BTreeMap<Pid, SocketState>,
    monitors: BTreeMap<Monitor, Token>,
    /// Live monitors and the process each watches.
    monitored: BTreeMap<Monitor, Pid>,
    messages: BTreeMap<Token, Message>,
    /// `:gb_trees` keyed by `{monotonic time, sequence}`; the sequence alone orders it.
    active: BTreeMap<u64, Pid>,
    blocked: BTreeMap<u64, Pid>,
    sequence: u64,
    active_websockets: i64,
    reserved_bytes: i64,
    inflight_bytes: i64,
    blocked_sources: i64,
    pressure: Option<Pressure>,
    pressure_recheck: bool,
    next_reference: u64,
    effects: Vec<Effect>,
}

impl Capacity {
    /// `init/1`: schedules the first check.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            ingress_limit: config.ingress_budget_bytes,
            ingress_weight: config.ingress_weight,
            watermark: config.memory_watermark_bytes,
            namespaces: BTreeMap::new(),
            connections: BTreeMap::new(),
            sockets: BTreeMap::new(),
            monitors: BTreeMap::new(),
            monitored: BTreeMap::new(),
            messages: BTreeMap::new(),
            active: BTreeMap::new(),
            blocked: BTreeMap::new(),
            sequence: 0,
            active_websockets: 0,
            reserved_bytes: 0,
            inflight_bytes: 0,
            blocked_sources: 0,
            pressure: None,
            pressure_recheck: false,
            next_reference: 0,
            effects: vec![Effect::ScheduleCheck],
        }
    }

    /// The effects queued since the last call, oldest first.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn make_ref(&mut self) -> u64 {
        self.next_reference += 1;
        self.next_reference
    }

    /// `handle_call({:admit_connection, namespace, limit, holder}, from, state)`.
    ///
    /// `holder_alive` is `node(holder) == node() and Process.alive?(holder)`; the caller is a
    /// running local process by construction.
    ///
    /// # Errors
    ///
    /// The reason of the `{:error, reason}` reply.
    pub fn admit_connection(
        &mut self,
        namespace: &str,
        limit: i64,
        holder: Pid,
        holder_alive: bool,
    ) -> Result<Token, AdmitConnectionError> {
        let namespace_state = self
            .namespaces
            .get(namespace)
            .cloned()
            .unwrap_or(NamespaceState { limit, active: 0 });
        if !holder_alive {
            return Err(AdmitConnectionError::Unavailable);
        }
        if self.pressure.is_some() {
            return Err(AdmitConnectionError::Pressure);
        }
        if namespace_state.limit != limit {
            return Err(AdmitConnectionError::ConfigurationMismatch);
        }
        if namespace_state.active >= limit {
            return Err(AdmitConnectionError::Capacity);
        }
        let token = Token(self.make_ref());
        let monitor = Monitor(self.make_ref());
        self.effects.push(Effect::StartReservationTimer(token));
        self.effects.push(Effect::Monitor {
            monitor,
            pid: holder,
        });
        self.namespaces.insert(
            namespace.to_owned(),
            NamespaceState {
                limit: namespace_state.limit,
                active: namespace_state.active + 1,
            },
        );
        self.connections.insert(
            token,
            Connection {
                namespace: namespace.to_owned(),
                holder,
                monitor,
                status: ConnectionStatus::Reservation,
            },
        );
        self.monitors.insert(monitor, token);
        self.monitored.insert(monitor, holder);
        Ok(token)
    }

    /// `handle_call({:attach_connection, token, socket}, from, state)`: `socket` is the caller.
    ///
    /// # Errors
    ///
    /// `{:error, :expired}`: no reservation for the token held by the caller.
    pub fn attach_connection(&mut self, token: Token, socket: Pid) -> Result<(), Expired> {
        let Some(connection) = self.connections.get(&token).cloned() else {
            return Err(Expired);
        };
        let ConnectionStatus::Reservation = connection.status else {
            return Err(Expired);
        };
        if connection.holder != socket {
            return Err(Expired);
        }
        self.effects.push(Effect::CancelReservationTimer(token));
        let active_key = self.next_key();
        let socket_state = SocketState {
            monitor: connection.monitor,
            connection: token,
            messages: BTreeSet::new(),
            active_key: Some(active_key),
            blocked_key: None,
            shedding: false,
        };
        if let Some(connection) = self.connections.get_mut(&token) {
            connection.status = ConnectionStatus::Active(socket);
        }
        self.sockets.insert(socket, socket_state);
        self.active.insert(active_key, socket);
        self.active_websockets += 1;
        Ok(())
    }

    /// `active_connections/1`.
    #[must_use]
    pub fn active_connections(&self, namespace: &str) -> i64 {
        self.namespaces
            .get(namespace)
            .map_or(0, |state| state.active)
    }

    /// `handle_call({:admit_message, socket, payload_bytes}, from, state)`: `socket` is the
    /// caller.
    ///
    /// # Errors
    ///
    /// The reason of the `{:error, reason}` reply.
    pub fn admit_message(
        &mut self,
        socket: Pid,
        payload_bytes: i64,
    ) -> Result<Token, AdmitMessageError> {
        let weighted_bytes = payload_bytes * self.ingress_weight;
        let Some(socket_state) = self.sockets.get(&socket) else {
            return Err(AdmitMessageError::Closed);
        };
        if socket_state.shedding {
            return Err(AdmitMessageError::Closed);
        }
        if self.pressure.is_some() {
            return Err(AdmitMessageError::Pressure);
        }
        if weighted_bytes > self.ingress_limit {
            return Err(AdmitMessageError::MessageExceedsBudget);
        }
        if self.reserved_bytes + weighted_bytes > self.ingress_limit {
            return Err(AdmitMessageError::BudgetExhausted);
        }
        let token = Token(self.make_ref());
        self.messages.insert(
            token,
            Message {
                socket,
                payload_bytes,
                weighted_bytes,
                status: MessageStatus::Reserved,
                started: None,
            },
        );
        if let Some(socket_state) = self.sockets.get_mut(&socket) {
            socket_state.messages.insert(token);
        }
        self.reserved_bytes += weighted_bytes;
        Ok(token)
    }

    /// `handle_call({:start_delivery, token, socket}, from, state)`: `socket` is the caller;
    /// `now_us` is the caller's monotonic clock in microseconds.
    ///
    /// # Errors
    ///
    /// The reason of the `{:error, reason}` reply.
    pub fn start_delivery(
        &mut self,
        token: Token,
        socket: Pid,
        now_us: i64,
    ) -> Result<(), StartDeliveryError> {
        let reserved = matches!(
            self.messages.get(&token),
            Some(message) if message.status == MessageStatus::Reserved && message.socket == socket
        );
        if !reserved {
            return Err(StartDeliveryError::Expired);
        }
        let Some(socket_state) = self.sockets.get(&socket).cloned() else {
            return Err(StartDeliveryError::Closed);
        };
        if socket_state.shedding {
            return Err(StartDeliveryError::Closed);
        }
        if self.pressure.is_some() {
            return Err(StartDeliveryError::Pressure);
        }
        let blocked_key = self.ensure_blocked(socket, &socket_state);
        if let Some(message) = self.messages.get_mut(&token) {
            message.status = MessageStatus::Delivering;
            message.started = Some(now_us);
            self.inflight_bytes += message.payload_bytes;
        }
        if let Some(socket_state) = self.sockets.get_mut(&socket) {
            socket_state.blocked_key = Some(blocked_key);
        }
        Ok(())
    }

    /// `finish_message/1`.
    pub fn finish_message(&mut self, token: Token, now_us: i64) {
        self.remove_message(token, true, now_us);
    }

    /// `cancel_message/1`.
    pub fn cancel_message(&mut self, token: Token) {
        self.remove_message(token, false, 0);
    }

    /// `release_connection/1`.
    pub fn release_connection(&mut self, token: Token) {
        self.release(token, true);
    }

    /// A `{:DOWN, monitor, :process, pid, reason}` for every monitor of `pid`, oldest first.
    pub fn process_down(&mut self, pid: Pid) {
        let monitors: Vec<Monitor> = self
            .monitored
            .iter()
            .filter(|(_, monitored)| **monitored == pid)
            .map(|(monitor, _)| *monitor)
            .collect();
        for monitor in monitors {
            self.monitor_down(monitor);
        }
    }

    /// One `{:DOWN, monitor, ...}` message.
    pub fn monitor_down(&mut self, monitor: Monitor) {
        self.monitored.remove(&monitor);
        if let Some(token) = self.monitors.get(&monitor).copied() {
            self.release(token, false);
        }
    }

    /// `handle_info({:expire, token}, state)`.
    pub fn expire(&mut self, token: Token) {
        if matches!(
            self.connections.get(&token),
            Some(Connection {
                status: ConnectionStatus::Reservation,
                ..
            })
        ) {
            self.release(token, true);
        }
    }

    /// `handle_info(:check, state)`.
    pub fn check(&mut self, memory: i64) {
        self.effects.push(Effect::ScheduleCheck);
        self.shed_if_needed(memory);
    }

    /// `handle_call(:check_now, ...)`.
    pub fn check_now(&mut self, memory: i64) {
        self.shed_if_needed(memory);
    }

    /// `handle_info(:pressure_recheck, state)`.
    pub fn pressure_recheck(&mut self, memory: i64) {
        self.pressure_recheck = false;
        self.shed_if_needed(memory);
    }

    /// `handle_call({:set_watermark, bytes}, ...)`.
    pub fn set_watermark(&mut self, bytes: i64) {
        self.watermark = bytes;
        if bytes == 0 {
            self.pressure = None;
        }
    }

    /// The gauges of `snapshot/0` and `value/1`.
    #[must_use]
    pub const fn gauges(&self) -> Gauges {
        Gauges {
            active_websockets: self.active_websockets,
            ingress_reserved_bytes: self.reserved_bytes,
            inflight_delivery_bytes: self.inflight_bytes,
            backpressured_sources: self.blocked_sources,
        }
    }

    /// `status/2`.
    #[must_use]
    pub fn status(&self, namespace: &str, limit: i64) -> Status {
        let namespace_state = self
            .namespaces
            .get(namespace)
            .cloned()
            .unwrap_or(NamespaceState { limit, active: 0 });
        let admission = if self.pressure.is_some() {
            Admission::Pressure
        } else if namespace_state.limit != limit {
            Admission::ConfigurationMismatch
        } else if namespace_state.active >= limit {
            Admission::ConnectionCapacity
        } else {
            Admission::Open
        };
        Status {
            admission,
            gauges: self.gauges(),
        }
    }

    /// The `pressure` field of the state.
    #[must_use]
    pub const fn pressure(&self) -> Option<Pressure> {
        self.pressure
    }

    /// Sockets in the `active` tree, oldest key first.
    #[must_use]
    pub fn active_order(&self) -> Vec<Pid> {
        self.active.values().copied().collect()
    }

    /// Sockets in the `blocked` tree, oldest key first.
    #[must_use]
    pub fn blocked_order(&self) -> Vec<Pid> {
        self.blocked.values().copied().collect()
    }

    /// The live connection, socket, message and monitor counts of the state maps.
    #[must_use]
    pub fn sizes(&self) -> [usize; 5] {
        [
            self.namespaces.len(),
            self.connections.len(),
            self.sockets.len(),
            self.messages.len(),
            self.monitors.len(),
        ]
    }

    fn release(&mut self, token: Token, demonitor: bool) {
        let Some(connection) = self.connections.get(&token).cloned() else {
            return;
        };
        match connection.status {
            ConnectionStatus::Reservation => {
                self.effects.push(Effect::CancelReservationTimer(token));
                if demonitor {
                    self.demonitor(connection.monitor);
                }
                self.monitors.remove(&connection.monitor);
                self.remove_connection_record(token);
            }
            ConnectionStatus::Active(socket) => self.remove_socket(socket, demonitor),
        }
    }

    fn demonitor(&mut self, monitor: Monitor) {
        self.monitored.remove(&monitor);
        self.effects.push(Effect::Demonitor(monitor));
    }

    fn remove_socket(&mut self, socket: Pid, demonitor: bool) {
        let Some(socket_state) = self.sockets.get(&socket).cloned() else {
            return;
        };
        if demonitor {
            self.demonitor(socket_state.monitor);
        }
        for token in &socket_state.messages {
            self.remove_message(*token, false, 0);
        }
        let socket_state = self.sockets.get(&socket).cloned().unwrap_or(socket_state);
        self.sockets.remove(&socket);
        self.monitors.remove(&socket_state.monitor);
        delete_key(&mut self.active, socket_state.active_key);
        delete_key(&mut self.blocked, socket_state.blocked_key);
        self.active_websockets -= 1;
        self.remove_connection_record(socket_state.connection);
    }

    fn remove_connection_record(&mut self, token: Token) {
        let Some(connection) = self.connections.remove(&token) else {
            return;
        };
        let namespace_state = self
            .namespaces
            .get_mut(&connection.namespace)
            .expect("a connection's namespace is tracked");
        namespace_state.active -= 1;
        if namespace_state.active == 0 {
            self.namespaces.remove(&connection.namespace);
        }
    }

    fn remove_message(&mut self, token: Token, observe_wait: bool, now_us: i64) {
        let Some(message) = self.messages.remove(&token) else {
            return;
        };
        let delivering = message.status == MessageStatus::Delivering;
        if observe_wait && delivering {
            self.effects.push(Effect::ObserveDeliveryWait {
                microseconds: now_us - message.started.unwrap_or(now_us),
            });
        }
        self.reserved_bytes -= message.weighted_bytes;
        if delivering {
            self.inflight_bytes -= message.payload_bytes;
        }
        let Some(mut socket_state) = self.sockets.get(&message.socket).cloned() else {
            return;
        };
        socket_state.messages.remove(&token);
        if delivering && !self.delivering_for_socket(&socket_state.messages) {
            let blocked_key = socket_state.blocked_key;
            socket_state.blocked_key = None;
            self.sockets.insert(message.socket, socket_state);
            delete_key(&mut self.blocked, blocked_key);
            self.blocked_sources -= 1;
        } else {
            self.sockets.insert(message.socket, socket_state);
        }
    }

    fn delivering_for_socket(&self, tokens: &BTreeSet<Token>) -> bool {
        tokens.iter().any(|token| {
            matches!(
                self.messages.get(token),
                Some(message) if message.status == MessageStatus::Delivering
            )
        })
    }

    fn ensure_blocked(&mut self, socket: Pid, socket_state: &SocketState) -> u64 {
        if let Some(key) = socket_state.blocked_key {
            return key;
        }
        let key = self.next_key();
        self.blocked.insert(key, socket);
        self.blocked_sources += 1;
        key
    }

    fn shed_if_needed(&mut self, memory: i64) {
        let recovery = recovery_threshold(self.watermark);
        if self.watermark == 0 || (self.pressure.is_some() && memory <= recovery) {
            self.pressure = None;
        } else if memory >= self.watermark || self.pressure.is_some() {
            let batch = pressure_batch(self.pressure, memory, recovery, self.watermark);
            let victims = self.shed_candidates(batch);
            self.pressure = Some(Pressure {
                memory,
                victims,
                batch,
            });
            self.schedule_pressure_recheck();
        }
    }

    fn schedule_pressure_recheck(&mut self) {
        if self.pressure_recheck {
            return;
        }
        if self.active.is_empty() && self.blocked.is_empty() {
            return;
        }
        self.effects.push(Effect::SchedulePressureRecheck);
        self.pressure_recheck = true;
    }

    fn shed_candidates(&mut self, mut remaining: i64) -> i64 {
        let mut victims = 0;
        while remaining > 0 {
            let Some(socket) = self.next_candidate() else {
                break;
            };
            self.effects.push(Effect::MemoryPressure(socket));
            self.effects.push(Effect::MemoryPressureDisconnect);
            remaining -= 1;
            victims += 1;
        }
        victims
    }

    fn next_candidate(&mut self) -> Option<Pid> {
        if let Some((_, socket)) = self.blocked.first_key_value().map(|(k, s)| (*k, *s)) {
            let socket_state = self
                .sockets
                .get_mut(&socket)
                .expect("a blocked socket is tracked");
            let (active_key, blocked_key) = (socket_state.active_key, socket_state.blocked_key);
            socket_state.active_key = None;
            socket_state.blocked_key = None;
            socket_state.shedding = true;
            delete_key(&mut self.active, active_key);
            delete_key(&mut self.blocked, blocked_key);
            return Some(socket);
        }
        if let Some((_, socket)) = self.active.last_key_value().map(|(k, s)| (*k, *s)) {
            let socket_state = self
                .sockets
                .get_mut(&socket)
                .expect("an active socket is tracked");
            let active_key = socket_state.active_key;
            socket_state.active_key = None;
            socket_state.shedding = true;
            delete_key(&mut self.active, active_key);
            return Some(socket);
        }
        None
    }

    fn next_key(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
}

fn delete_key(tree: &mut BTreeMap<u64, Pid>, key: Option<u64>) {
    if let Some(key) = key {
        tree.remove(&key);
    }
}

fn maximum_message() -> i128 {
    i128::try_from(MAXIMUM_MESSAGE_PAYLOAD_BYTES).expect("the limit fits")
}

fn recovery_threshold(watermark: i64) -> i64 {
    if watermark == 0 {
        0
    } else {
        i64::try_from((i128::from(watermark) - maximum_message()).max(0)).expect("fits")
    }
}

/// `pressure_batch/4`; Elixir `div` truncates toward zero, as Rust does.
fn pressure_batch(previous: Option<Pressure>, memory: i64, recovery: i64, watermark: i64) -> i64 {
    let memory = i128::from(memory);
    let batch = match previous {
        None => ((memory - i128::from(watermark) + (maximum_message() - 1)) / maximum_message())
            .clamp(1, INITIAL_MAX_SHED_BATCH),
        Some(previous) => {
            let relief = i128::from(previous.memory) - memory;
            if relief > 0 && previous.victims > 0 {
                let bytes_per_victim = (relief / i128::from(previous.victims)).max(1);
                ((memory - i128::from(recovery) + (bytes_per_victim - 1)) / bytes_per_victim)
                    .clamp(1, MAX_SHED_BATCH)
            } else {
                (i128::from(previous.batch) * 2).clamp(1, MAX_SHED_BATCH)
            }
        }
    };
    i64::try_from(batch).expect("a batch is small")
}
