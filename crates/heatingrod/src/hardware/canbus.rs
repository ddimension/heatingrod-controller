//! SOREL SCBI CAN protocol decode — 1:1 port of `sorel_canbus/__init__.py`.
//!
//! Pure decode over (can_id, data) pairs; the slcan transport plugs in
//! during Phase 5 wiring. Includes the AirInPipeDetector (warm-water
//! temperature drop >10°C in 5s while the pump runs).

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

// ---- protocol enums ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScbiProg {
    Controller = 0x0B,
    Datalogger = 0x80,
    Remotesensor = 0x83,
    Namedsensors = 0x84,
    Hcc = 0x85,
    Availres = 0x8C,
    Paramsync = 0x90,
    Roomsync = 0x91,
    Msglog = 0x94,
    Cbcs = 0x95,
}

impl ScbiProg {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x0B => Self::Controller,
            0x80 => Self::Datalogger,
            0x83 => Self::Remotesensor,
            0x84 => Self::Namedsensors,
            0x85 => Self::Hcc,
            0x8C => Self::Availres,
            0x90 => Self::Paramsync,
            0x91 => Self::Roomsync,
            0x94 => Self::Msglog,
            0x95 => Self::Cbcs,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScbiMsg {
    Request = 0x00,
    Reserve = 0x01,
    Response = 0x02,
    Error = 0x03,
}

impl ScbiMsg {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x00 => Self::Request,
            0x01 => Self::Reserve,
            0x02 => Self::Response,
            0x03 => Self::Error,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Request => "REQUEST",
            Self::Reserve => "RESERVE",
            Self::Response => "RESPONSE",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlgFunc {
    Undefined = 0x00,
    Sensor = 0x01,
    Relay = 0x02,
    HydraulicProgram = 0x03,
    ErrorMessage = 0x04,
    ParamMonitoring = 0x05,
    Statistic = 0x06,
    Overview = 0x07,
    HydraulicConfig = 0x08,
}

impl DlgFunc {
    fn name(self) -> &'static str {
        match self {
            Self::Undefined => "UNDEFINED",
            Self::Sensor => "SENSOR",
            Self::Relay => "RELAY",
            Self::HydraulicProgram => "HYDRAULIC_PROGRAM",
            Self::ErrorMessage => "ERROR_MESSAGE",
            Self::ParamMonitoring => "PARAM_MONITORING",
            Self::Statistic => "STATISTIC",
            Self::Overview => "OVERVIEW",
            Self::HydraulicConfig => "HYDRAULIC_CONFIG",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HccFunc {
    Heatrequest = 0x00,
    HcState1 = 0x01,
    HcState2 = 0x02,
    HcState3 = 0x03,
    HcState4 = 0x04,
}

impl HccFunc {
    fn name(self) -> &'static str {
        match self {
            Self::Heatrequest => "HEATREQUEST",
            Self::HcState1 => "HC_STATE1",
            Self::HcState2 => "HC_STATE2",
            Self::HcState3 => "HC_STATE3",
            Self::HcState4 => "HC_STATE4",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrFunc {
    HasAnybody = 0x00,
    IAmHere = 0x01,
    GetCtrId = 0x02,
    GetPrograms = 0x03,
    AddProgram = 0x04,
    RemoveProgram = 0x05,
    GetDatetime = 0x06,
    SetDatetime = 0x07,
    IAmReset = 0x08,
    DlgTest = 0x09,
}

impl CtrFunc {
    fn name(self) -> &'static str {
        match self {
            Self::HasAnybody => "HAS_ANYBODY",
            Self::IAmHere => "I_AM_HERE",
            Self::GetCtrId => "GET_CTR_ID",
            Self::GetPrograms => "GET_PROGRAMS",
            Self::AddProgram => "ADD_PROGRAM",
            Self::RemoveProgram => "REMOVE_PROGRAM",
            Self::GetDatetime => "GET_DATETIME",
            Self::SetDatetime => "SET_DATETIME",
            Self::IAmReset => "I_AM_RESET",
            Self::DlgTest => "DLG_TEST",
        }
    }
}

const SENSOR_OVERFLOW: f64 = 3276.0; // 0x7FEC/10 — sensor not connected

fn byte2temp(x: u8) -> f64 {
    // Python: (x * 100) // 255 — integer floor division.
    ((x as u64 * 100) / 255) as f64
}

/// Python `BYTE2TEMP` applied to a full u16 (HC_STATE4 fields).
fn byte2temp_u16(x: u16) -> f64 {
    ((x as u64 * 100) / 255) as f64
}

// ---- SCBI id ----

#[derive(Debug, Clone)]
pub struct ScbiId {
    pub prog: u8,
    pub client: u8,
    pub func: u8,
    pub prot: u8,
    pub msg: u8,
    pub raw_id: u32,
}

impl ScbiId {
    pub fn from_can_id(can_id: u32) -> Self {
        let b = can_id.to_le_bytes();
        Self {
            prog: b[0],
            client: b[1],
            func: b[2],
            prot: b[3] & 0x07,
            msg: (b[3] >> 3) & 0x03,
            raw_id: can_id,
        }
    }

    pub fn prog_name(&self) -> String {
        match ScbiProg::from_u8(self.prog) {
            Some(p) => format!("{p:?}").to_uppercase(),
            None => format!("UNKNOWN_0x{:02X}", self.prog),
        }
    }

    pub fn msg_name(&self) -> String {
        match ScbiMsg::from_u8(self.msg) {
            Some(m) => m.name().to_string(),
            None => format!("MSG_{}", self.msg),
        }
    }

    pub fn func_name(&self) -> String {
        match ScbiProg::from_u8(self.prog) {
            Some(ScbiProg::Datalogger) => {
                if let Some(f) = (0x00..=0x08)
                    .find_map(|v| (v == self.func).then(|| dlg_func_of(v)))
                {
                    return f.name().to_string();
                }
            }
            Some(ScbiProg::Hcc) => {
                if let Some(f) = (0x00..=0x04).find_map(|v| (v == self.func).then(|| hcc_func_of(v))) {
                    return f.name().to_string();
                }
            }
            Some(ScbiProg::Controller) => {
                if let Some(f) =
                    (0x00..=0x09).find_map(|v| (v == self.func).then(|| ctr_func_of(v)))
                {
                    return f.name().to_string();
                }
            }
            _ => {}
        }
        format!("FUNC_0x{:02X}", self.func)
    }
}

fn dlg_func_of(v: u8) -> DlgFunc {
    match v {
        0x01 => DlgFunc::Sensor,
        0x02 => DlgFunc::Relay,
        0x03 => DlgFunc::HydraulicProgram,
        0x04 => DlgFunc::ErrorMessage,
        0x05 => DlgFunc::ParamMonitoring,
        0x06 => DlgFunc::Statistic,
        0x07 => DlgFunc::Overview,
        0x08 => DlgFunc::HydraulicConfig,
        _ => DlgFunc::Undefined,
    }
}

fn hcc_func_of(v: u8) -> HccFunc {
    match v {
        0x00 => HccFunc::Heatrequest,
        0x01 => HccFunc::HcState1,
        0x02 => HccFunc::HcState2,
        0x03 => HccFunc::HcState3,
        _ => HccFunc::HcState4,
    }
}

fn ctr_func_of(v: u8) -> CtrFunc {
    match v {
        0x00 => CtrFunc::HasAnybody,
        0x01 => CtrFunc::IAmHere,
        0x02 => CtrFunc::GetCtrId,
        0x03 => CtrFunc::GetPrograms,
        0x04 => CtrFunc::AddProgram,
        0x05 => CtrFunc::RemoveProgram,
        0x06 => CtrFunc::GetDatetime,
        0x07 => CtrFunc::SetDatetime,
        0x08 => CtrFunc::IAmReset,
        _ => CtrFunc::DlgTest,
    }
}

// ---- decoded message ----

#[derive(Debug, Clone)]
pub struct DecodedMessage {
    pub timestamp: f64,
    pub scbi_id: ScbiId,
    pub raw_data: Vec<u8>,
    pub decoded: bool,
    pub category: String,
    pub topic: String,
    pub value: Option<f64>,
    pub details: HashMap<String, Value>,
}

impl DecodedMessage {
    fn new(timestamp: f64, scbi_id: ScbiId, raw_data: &[u8]) -> Self {
        Self {
            timestamp,
            scbi_id,
            raw_data: raw_data.to_vec(),
            decoded: false,
            category: "unknown".into(),
            topic: String::new(),
            value: None,
            details: HashMap::new(),
        }
    }
}

// ---- air-in-pipe detector ----

pub struct AirInPipeDetector {
    temp_drop_threshold: f64,
    time_window: f64,
    min_active_temp: f64,
    cooldown: f64,
    warmwater_history: VecDeque<(f64, f64)>, // (timestamp, temp), maxlen 50
    pump_pwm: u8,
    last_alert_ts: f64,
}

impl Default for AirInPipeDetector {
    fn default() -> Self {
        Self::new(10.0, 5.0, 50.0, 120.0)
    }
}

impl AirInPipeDetector {
    pub fn new(
        temp_drop_threshold: f64,
        time_window: f64,
        min_active_temp: f64,
        cooldown: f64,
    ) -> Self {
        Self {
            temp_drop_threshold,
            time_window,
            min_active_temp,
            cooldown,
            warmwater_history: VecDeque::with_capacity(50),
            pump_pwm: 0,
            last_alert_ts: 0.0,
        }
    }

    pub fn update_warmwater(&mut self, timestamp: f64, temp: f64) {
        if self.warmwater_history.len() >= 50 {
            self.warmwater_history.pop_front();
        }
        self.warmwater_history.push_back((timestamp, temp));
    }

    pub fn update_pump_pwm(&mut self, value: u8) {
        self.pump_pwm = value;
    }

    /// Python `check()`: drop ≥ threshold within the window, warm enough,
    /// pump running, cooldown expired.
    pub fn check(&mut self) -> Option<AirAlert> {
        if self.warmwater_history.len() < 3 {
            return None;
        }
        let (now, current_temp) = *self.warmwater_history.back().unwrap();
        let mut max_recent_temp = current_temp;
        for (ts, temp) in &self.warmwater_history {
            if now - ts <= self.time_window {
                max_recent_temp = max_recent_temp.max(*temp);
            }
        }
        let drop = max_recent_temp - current_temp;
        if drop >= self.temp_drop_threshold
            && max_recent_temp >= self.min_active_temp
            && self.pump_pwm > 10
            && now - self.last_alert_ts > self.cooldown
        {
            self.last_alert_ts = now;
            return Some(AirAlert {
                temp_drop: drop,
                from_temp: max_recent_temp,
                to_temp: current_temp,
                pump_pwm: self.pump_pwm,
            });
        }
        None
    }
}

#[derive(Debug, PartialEq)]
pub struct AirAlert {
    pub temp_drop: f64,
    pub from_temp: f64,
    pub to_temp: f64,
    pub pump_pwm: u8,
}

// ---- decoder ----

pub struct SorelDecoder {
    pub sensor_names: HashMap<u8, String>,
    pub sensor_values: HashMap<u8, f64>,
    pub stats: Stats,
    pub air_detector: AirInPipeDetector,
    unknown_buffer: usize,
    unknown_messages: VecDeque<DecodedMessage>,
    seen_unknown: std::collections::HashSet<(u8, u8)>,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub total: u64,
    pub decoded: u64,
    pub unknown: u64,
    pub errors: u64,
}

impl Default for SorelDecoder {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl SorelDecoder {
    pub fn new(unknown_buffer_size: usize) -> Self {
        Self {
            sensor_names: HashMap::from([
                (0, "Kaltwasser".to_string()),
                (1, "Zirkulation".to_string()),
                (6, "Warmwasser".to_string()),
                (7, "Durchfluss_VFS".to_string()),
            ]),
            sensor_values: HashMap::new(),
            stats: Stats::default(),
            air_detector: AirInPipeDetector::default(),
            unknown_buffer: unknown_buffer_size,
            unknown_messages: VecDeque::new(),
            seen_unknown: std::collections::HashSet::new(),
        }
    }

    pub fn decode(&mut self, can_id: u32, data: &[u8], timestamp: f64) -> DecodedMessage {
        self.stats.total += 1;
        let scbi_id = ScbiId::from_can_id(can_id);
        let mut msg = DecodedMessage::new(timestamp, scbi_id, data);

        if msg.scbi_id.msg == ScbiMsg::Error as u8 {
            msg.category = "error".into();
            msg.details.insert("error".into(), Value::from("CAN error frame"));
            self.stats.errors += 1;
            return msg;
        }

        match ScbiProg::from_u8(msg.scbi_id.prog) {
            Some(ScbiProg::Datalogger) if msg.scbi_id.msg == ScbiMsg::Response as u8 => {
                self.decode_datalogger(&mut msg);
            }
            Some(ScbiProg::Hcc) => self.decode_hcc(&mut msg),
            Some(ScbiProg::Controller) => self.decode_controller(&mut msg),
            _ => {
                msg.category = "unknown_prog".into();
                msg.details
                    .insert("prog".into(), Value::from(msg.scbi_id.prog_name()));
            }
        }

        if !msg.decoded {
            self.stats.unknown += 1;
            self.push_unknown(msg.clone());
        } else {
            self.stats.decoded += 1;
        }
        msg
    }

    fn push_unknown(&mut self, msg: DecodedMessage) {
        if self.unknown_messages.len() >= self.unknown_buffer {
            self.unknown_messages.pop_front();
        }
        self.unknown_messages.push_back(msg);
    }

    fn decode_datalogger(&mut self, msg: &mut DecodedMessage) {
        let func = msg.scbi_id.func;
        let data = msg.raw_data.clone();
        match dlg_func_of(func) {
            DlgFunc::Sensor if data.len() >= 3 => self.decode_sensor(msg, &data),
            DlgFunc::Relay if data.len() >= 3 => self.decode_relay(msg, &data),
            DlgFunc::Overview if data.len() >= 7 => self.decode_overview(msg, &data),
            DlgFunc::Statistic if data.len() >= 2 => self.decode_statistic(msg, &data),
            _ => {
                msg.category = "datalogger_unsupported".into();
                msg.details.insert(
                    "func".into(),
                    Value::from(msg.scbi_id.func_name()),
                );
                msg.details
                    .insert("data_len".into(), Value::from(data.len()));
            }
        }
    }

    fn decode_sensor(&mut self, msg: &mut DecodedMessage, data: &[u8]) {
        let sensor_id = data[0];
        let value_raw = i16::from_le_bytes([data[1], data[2]]) as f64;
        let sensor_type = data.get(3).copied().unwrap_or(0);
        let subtype = data.get(4).copied().unwrap_or(0);
        let value = value_raw / 10.0;

        // Overflow: sensor not connected
        if (value - SENSOR_OVERFLOW).abs() < 1.0 {
            msg.decoded = true;
            msg.category = "sensor_overflow".into();
            msg.details.insert("sensor_id".into(), Value::from(sensor_id));
            msg.details.insert("raw".into(), Value::from(value_raw));
            return;
        }

        let name = self
            .sensor_names
            .get(&sensor_id)
            .cloned()
            .unwrap_or_else(|| sensor_id.to_string());
        self.sensor_values.insert(sensor_id, value);

        msg.decoded = true;
        msg.category = "sensor".into();
        msg.topic = format!("ltdc/DLG_SENSOR/{name}");
        msg.value = Some(value);
        msg.details.insert("sensor_id".into(), Value::from(sensor_id));
        msg.details.insert("name".into(), Value::from(name));
        msg.details.insert("value".into(), Value::from(value));
        msg.details.insert(
            "unit".into(),
            Value::from(if sensor_type == 0 || sensor_type == 4 { "°C" } else { "" }),
        );
        msg.details
            .insert("sensor_type".into(), Value::from(sensor_type));
        msg.details.insert("subtype".into(), Value::from(subtype));

        // Feed the air-in-pipe detector (sensor 6 = Warmwasser)
        if sensor_id == 6 {
            self.air_detector.update_warmwater(msg.timestamp, value);
        }
    }

    fn decode_relay(&mut self, msg: &mut DecodedMessage, data: &[u8]) {
        let relay_id = data[0];
        let mode = data[1];
        let value = data[2];
        let exfunc: Vec<u8> = if data.len() >= 5 {
            data[3..5].to_vec()
        } else {
            vec![]
        };

        let mode_name = match mode {
            0 => "switched",
            1 => "phase",
            2 => "pwm",
            3 => "voltage",
            _ => "unknown",
        };
        let topic_mode = mode_name;
        msg.decoded = true;
        msg.category = "relay".into();
        msg.topic = format!("ltdc/DLG_RELAY/{relay_id}/{topic_mode}");
        msg.value = Some(value as f64);

        // Feed the detector with mixer pump PWM (relay 3)
        if relay_id == 3 && mode == 2 {
            self.air_detector.update_pump_pwm(value);
        }
        msg.details.insert("relay_id".into(), Value::from(relay_id));
        msg.details.insert("mode".into(), Value::from(mode_name));
        msg.details.insert("value".into(), Value::from(value));
        msg.details.insert("exfunc".into(), Value::from(exfunc));
    }

    fn decode_overview(&mut self, msg: &mut DecodedMessage, data: &[u8]) {
        let flags = data[0];
        let ov_type = (flags >> 5) & 0x07;
        let idx = flags & 0x1F;
        let ov_mode = data[1];
        let hours = u16::from_le_bytes([data[2], data[3]]);
        let heat_yield = if data.len() >= 8 {
            u32::from_le_bytes([data[4], data[5], data[6], data[7]])
        } else {
            0
        };

        let topic_name = match ov_type {
            1 => "tag",
            2 => "woche",
            3 => "monat",
            4 => "jahr",
            5 => "gesamt",
            _ => "unknown",
        };
        msg.decoded = true;
        msg.category = "overview".into();
        msg.details.insert("type".into(), Value::from(ov_type));
        msg.details.insert("index".into(), Value::from(idx));
        msg.details.insert("mode".into(), Value::from(ov_mode));
        msg.details.insert("hours".into(), Value::from(hours));
        msg.details
            .insert("heat_yield_kwh".into(), Value::from(heat_yield));

        // Only current period (idx=0) publishes to the legacy topic
        if idx == 0 {
            msg.topic = format!("ltdc/DLG_OVERVIEW/{topic_name}");
            msg.value = Some(heat_yield as f64);
        } else {
            msg.topic = String::new();
            msg.value = None;
        }
    }

    fn decode_statistic(&mut self, msg: &mut DecodedMessage, data: &[u8]) {
        let stat_id = data[0];
        let value = if data.len() >= 3 {
            i16::from_le_bytes([data[1], data[2]]) as f64
        } else {
            data[1] as f64
        };
        let name = match stat_id {
            0 => "Leistung_VFS1",
            2 => "Waermeleistung",
            _ => "",
        };
        let name = if name.is_empty() {
            stat_id.to_string()
        } else {
            name.to_string()
        };

        msg.decoded = true;
        msg.category = "statistic".into();
        msg.topic = format!("ltdc/DLG_STATISTIC/{name}");
        msg.value = Some(value);
        msg.details.insert("stat_id".into(), Value::from(stat_id));
        msg.details.insert("name".into(), Value::from(name.clone()));
        msg.details.insert("value".into(), Value::from(value));
    }

    fn decode_hcc(&mut self, msg: &mut DecodedMessage) {
        let func = msg.scbi_id.func;
        let data = msg.raw_data.clone();
        msg.category = "hcc".into();

        match hcc_func_of(func) {
            HccFunc::Heatrequest if data.len() >= 2 => {
                let raw_temp = data[0];
                let heatsource = data[1];
                let temp = byte2temp(raw_temp);
                msg.decoded = true;
                msg.topic = "ltdc/HCC/heatrequest".into();
                msg.value = Some(temp);
                msg.details.insert("temp_request".into(), Value::from(temp));
                msg.details.insert(
                    "heatsource".into(),
                    Value::from(if heatsource == 0 {
                        "conventional"
                    } else {
                        "solar"
                    }),
                );
                msg.details.insert("raw_temp".into(), Value::from(raw_temp));
            }
            HccFunc::HcState1 if data.len() >= 5 => {
                let circuit = data[0];
                let state = data[1];
                let temp_flowset = byte2temp(data[2]);
                let temp_flow = byte2temp(data[3]);
                let temp_storage = byte2temp(data[4]);
                msg.decoded = true;
                msg.topic = format!("ltdc/HCC/circuit{circuit}/state1");
                msg.value = Some(temp_flow);
                msg.details.insert("circuit".into(), Value::from(circuit));
                msg.details.insert("state".into(), Value::from(state));
                msg.details
                    .insert("temp_flow_set".into(), Value::from(temp_flowset));
                msg.details
                    .insert("temp_flow_actual".into(), Value::from(temp_flow));
                msg.details
                    .insert("temp_storage".into(), Value::from(temp_storage));
            }
            HccFunc::HcState2 if data.len() >= 5 => {
                let circuit = data[0];
                let wheel = data[1];
                let temp_set = byte2temp(data[2]);
                let temp_room = byte2temp(data[3]);
                let humidity = data[4];
                msg.decoded = true;
                msg.topic = format!("ltdc/HCC/circuit{circuit}/state2");
                msg.value = Some(temp_room);
                msg.details.insert("circuit".into(), Value::from(circuit));
                msg.details.insert("wheel".into(), Value::from(wheel));
                msg.details
                    .insert("temp_room_set".into(), Value::from(temp_set));
                msg.details
                    .insert("temp_room_actual".into(), Value::from(temp_room));
                msg.details.insert("humidity".into(), Value::from(humidity));
            }
            HccFunc::HcState3 if data.len() >= 5 => {
                let circuit = data[0];
                let op_mode = data[1];
                let dewpoint = byte2temp(data[2]);
                let pump = data[3];
                let on_reason = data[4];
                msg.decoded = true;
                msg.topic = format!("ltdc/HCC/circuit{circuit}/state3");
                msg.value = Some(op_mode as f64);
                msg.details.insert("circuit".into(), Value::from(circuit));
                msg.details.insert("op_mode".into(), Value::from(op_mode));
                msg.details.insert("dewpoint".into(), Value::from(dewpoint));
                msg.details.insert("pump".into(), Value::from(pump));
                msg.details.insert("on_reason".into(), Value::from(on_reason));
            }
            HccFunc::HcState4 if data.len() >= 5 => {
                let circuit = data[0];
                // Python: BYTE2TEMP over the FULL u16 fields (the earlier
                // &0xFF mask diverged for high bytes ≠ 0 — 04.09.2026 audit).
                let temp_min = if data.len() >= 4 {
                    byte2temp_u16(u16::from_le_bytes([data[2], data[3]]))
                } else {
                    0.0
                };
                let temp_max = if data.len() >= 6 {
                    byte2temp_u16(u16::from_le_bytes([data[4], data[5]]))
                } else {
                    0.0
                };
                msg.decoded = true;
                msg.topic = format!("ltdc/HCC/circuit{circuit}/state4");
                msg.value = Some(temp_max);
                msg.details.insert("circuit".into(), Value::from(circuit));
                msg.details.insert("temp_min".into(), Value::from(temp_min));
                msg.details.insert("temp_max".into(), Value::from(temp_max));
            }
            _ => {
                // Known HCC functions with short/unexpected data — mark
                // decoded to suppress log spam (Python behavior).
                msg.decoded = true;
                msg.details
                    .insert("func".into(), Value::from(msg.scbi_id.func_name()));
                msg.details
                    .insert("data_len".into(), Value::from(data.len()));
                msg.details.insert("data".into(), Value::from(hex_of(&data)));
            }
        }
    }

    fn decode_controller(&mut self, msg: &mut DecodedMessage) {
        let func = msg.scbi_id.func;
        let data = msg.raw_data.clone();
        msg.category = "controller".into();

        match ctr_func_of(func) {
            CtrFunc::IAmHere if data.len() >= 4 => {
                msg.decoded = true;
                msg.topic = "ltdc/CTR/i_am_here".into();
                msg.details.insert("can_id".into(), Value::from(data[0]));
                msg.details.insert("dev_id".into(), Value::from(data[1]));
                msg.details.insert("oem_id".into(), Value::from(data[2]));
                msg.details.insert("variant".into(), Value::from(data[3]));
            }
            CtrFunc::HasAnybody if !data.is_empty() => {
                msg.decoded = true;
                msg.topic = "ltdc/CTR/has_anybody".into();
                msg.details.insert("can_id".into(), Value::from(data[0]));
            }
            CtrFunc::IAmReset if !data.is_empty() => {
                msg.decoded = true;
                msg.topic = "ltdc/CTR/reset".into();
                msg.details.insert("can_id".into(), Value::from(data[0]));
            }
            _ => {
                msg.details
                    .insert("func".into(), Value::from(msg.scbi_id.func_name()));
                msg.details.insert("data".into(), Value::from(hex_of(&data)));
            }
        }
    }

    pub fn get_unknown_messages(&self) -> &VecDeque<DecodedMessage> {
        &self.unknown_messages
    }

    pub fn get_sensor_values(&self) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        for (k, v) in &self.sensor_values {
            out.insert(
                self.sensor_names.get(k).cloned().unwrap_or_else(|| k.to_string()),
                *v,
            );
        }
        out
    }
}

fn hex_of(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- tests (ports of test_canbus.py) ----

#[cfg(test)]
mod tests {
    use super::*;

    fn can_id(prog: u8, client: u8, func: u8, prot: u8, msg: u8) -> u32 {
        let flags = (prot & 0x07) | ((msg & 0x03) << 3);
        u32::from_le_bytes([prog, client, func, flags])
    }

    const DATALOGGER: u8 = 0x80;
    const HCC: u8 = 0x85;
    const CONTROLLER: u8 = 0x0B;
    const RESPONSE: u8 = 0x02;

    #[test]
    fn scbi_id_decode_datalogger_sensor() {
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        let sid = ScbiId::from_can_id(id);
        assert_eq!(sid.prog, DATALOGGER);
        assert_eq!(sid.func, 0x01);
        assert_eq!(sid.msg, RESPONSE);
        assert_eq!(sid.prog_name(), "DATALOGGER");
        assert_eq!(sid.func_name(), "SENSOR");
        assert_eq!(sid.msg_name(), "RESPONSE");
    }

    #[test]
    fn scbi_id_decode_hcc() {
        let id = can_id(HCC, 0x16, 0x01, 0, RESPONSE);
        let sid = ScbiId::from_can_id(id);
        assert_eq!(sid.prog, HCC);
        assert_eq!(sid.func_name(), "HC_STATE1");
    }

    #[test]
    fn scbi_id_decode_controller() {
        let id = can_id(CONTROLLER, 0x00, 0x01, 0, RESPONSE);
        let sid = ScbiId::from_can_id(id);
        assert_eq!(sid.prog, CONTROLLER);
        assert_eq!(sid.func_name(), "I_AM_HERE");
    }

    #[test]
    fn scbi_id_unknown_prog() {
        let id = can_id(0xAA, 0x00, 0x00, 0, RESPONSE);
        let sid = ScbiId::from_can_id(id);
        assert!(sid.prog_name().contains("UNKNOWN"));
    }

    fn make_bus() -> SorelDecoder {
        let mut d = SorelDecoder::new(1000);
        d.sensor_names = HashMap::from([
            (0, "Kaltwasser".to_string()),
            (6, "Warmwasser".to_string()),
        ]);
        d
    }

    #[test]
    fn temperature_sensor() {
        let mut bus = make_bus();
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        let data = [6, 44, 1, 0, 0]; // 300 LE = 0x012C → [0x2C, 0x01]
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.category, "sensor");
        assert_eq!(decoded.topic, "ltdc/DLG_SENSOR/Warmwasser");
        assert_eq!(decoded.value, Some(30.0));
        assert_eq!(decoded.details["sensor_id"], Value::from(6));
    }

    #[test]
    fn overflow_sensor() {
        let mut bus = make_bus();
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        // 32760 = 0x7FF8 → [0xF8, 0x7F]
        let data = [2, 0xF8, 0x7F, 0, 0];
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.category, "sensor_overflow");
    }

    #[test]
    fn unknown_sensor_id() {
        let mut bus = make_bus();
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        let data = [99, 0xFA, 0x00, 0, 0]; // 250
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.details["name"], Value::from("99"));
        assert_eq!(decoded.topic, "ltdc/DLG_SENSOR/99");
    }

    #[test]
    fn switched_relay() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(DATALOGGER, 0x16, 0x02, 0, RESPONSE);
        let data = [0, 0, 255, 0, 0];
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.category, "relay");
        assert_eq!(decoded.topic, "ltdc/DLG_RELAY/0/switched");
        assert_eq!(decoded.value, Some(255.0));
    }

    #[test]
    fn pwm_relay() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(DATALOGGER, 0x16, 0x02, 0, RESPONSE);
        let data = [3, 2, 128, 0, 0];
        let decoded = bus.decode(id, &data, 0.0);
        assert_eq!(decoded.topic, "ltdc/DLG_RELAY/3/pwm");
        assert_eq!(decoded.value, Some(128.0));
    }

    #[test]
    fn overview_total() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(DATALOGGER, 0x16, 0x07, 0, RESPONSE);
        let flags = (5u8 << 5) | 0; // TOTAL, idx=0
        let mut data = vec![flags, 0];
        data.extend_from_slice(&1000u16.to_le_bytes());
        data.extend_from_slice(&12448u32.to_le_bytes());
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.category, "overview");
        assert_eq!(decoded.topic, "ltdc/DLG_OVERVIEW/gesamt");
    }

    #[test]
    fn heat_request() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(HCC, 0x16, 0x00, 0, RESPONSE);
        let data = [128, 0];
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.category, "hcc");
        assert_eq!(decoded.topic, "ltdc/HCC/heatrequest");
        assert_eq!(decoded.details["heatsource"], Value::from("conventional"));
        assert_eq!(decoded.details["temp_request"], Value::from(50.0));
    }

    #[test]
    fn hc_state1() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(HCC, 0x16, 0x01, 0, RESPONSE);
        let data = [1, 0x02, 128, 120, 200];
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.details["circuit"], Value::from(1));
        assert_eq!(decoded.details["temp_flow_set"], Value::from(50.0));
        assert!(decoded.details["temp_storage"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn i_am_here() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(CONTROLLER, 0x00, 0x01, 0, RESPONSE);
        let data = [0x16, 0x00, 0xBE, 0x02, 0, 0, 0, 0];
        let decoded = bus.decode(id, &data, 0.0);
        assert!(decoded.decoded);
        assert_eq!(decoded.details["oem_id"], Value::from(0xBE));
    }

    #[test]
    fn publishes_to_callback() {
        let mut bus = make_bus();
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        let data = [6, 44, 1, 0, 0];
        let decoded = bus.decode(id, &data, 0.0);
        let mut calls: Vec<(String, String, f64)> = Vec::new();
        if !decoded.topic.is_empty()
            && let Some(value) = decoded.value
        {
            calls.push((decoded.category.clone(), decoded.topic.clone(), value));
        }
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "sensor");
        assert_eq!(calls[0].1, "ltdc/DLG_SENSOR/Warmwasser");
        assert_eq!(calls[0].2, 30.0);
    }

    #[test]
    fn stats_tracking() {
        let mut bus = SorelDecoder::new(1000);
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        bus.decode(id, &[0, 0xFA, 0x00, 0, 0], 0.0); // known sensor
        let unk = can_id(0xFE, 0x00, 0x00, 0, RESPONSE);
        bus.decode(unk, &[0], 0.0); // unknown prog
        assert_eq!(bus.stats.total, 2);
        assert_eq!(bus.stats.decoded, 1);
        assert_eq!(bus.stats.unknown, 1);
    }

    #[test]
    fn unknown_buffer() {
        let mut bus = SorelDecoder::new(5);
        let unk = can_id(0xFE, 0x00, 0x00, 0, RESPONSE);
        for i in 0..10 {
            bus.decode(unk, &[i], 0.0);
        }
        assert_eq!(bus.get_unknown_messages().len(), 5);
    }

    #[test]
    fn sensor_values_map() {
        let mut bus = make_bus();
        let id = can_id(DATALOGGER, 0x16, 0x01, 0, RESPONSE);
        bus.decode(id, &[6, 0x27, 0x01, 0, 0], 0.0); // 295
        let vals = bus.get_sensor_values();
        assert_eq!(vals["Warmwasser"], 29.5);
    }

    // ---- AirInPipeDetector ----

    #[test]
    fn no_alert_when_cold() {
        let mut d = AirInPipeDetector::default();
        let now = 1000.0;
        d.update_warmwater(now, 30.0);
        d.update_warmwater(now + 1.0, 25.0);
        d.update_pump_pwm(100);
        assert!(d.check().is_none()); // below min_active_temp
    }

    #[test]
    fn no_alert_normal_operation() {
        let mut d = AirInPipeDetector::default();
        let now = 1000.0;
        d.update_pump_pwm(100);
        for i in 0..10 {
            d.update_warmwater(now + i as f64, 55.0 + i as f64);
        }
        assert!(d.check().is_none());
    }

    #[test]
    fn alert_on_sudden_drop() {
        let mut d = AirInPipeDetector::new(10.0, 5.0, 50.0, 0.0);
        let now = 1000.0;
        d.update_pump_pwm(150);
        d.update_warmwater(now, 65.0);
        d.update_warmwater(now + 1.0, 63.0);
        d.update_warmwater(now + 2.0, 50.0);
        let alert = d.check().expect("alert");
        assert_eq!(alert.temp_drop, 15.0);
        assert_eq!(alert.from_temp, 65.0);
        assert_eq!(alert.to_temp, 50.0);
        assert_eq!(alert.pump_pwm, 150);
    }

    #[test]
    fn no_alert_pump_off() {
        let mut d = AirInPipeDetector::new(10.0, 5.0, 50.0, 0.0);
        let now = 1000.0;
        d.update_pump_pwm(0);
        d.update_warmwater(now, 65.0);
        d.update_warmwater(now + 2.0, 50.0);
        assert!(d.check().is_none());
    }

    #[test]
    fn cooldown_prevents_spam() {
        let mut d = AirInPipeDetector::new(10.0, 5.0, 50.0, 120.0);
        let now = 1000.0;
        d.update_pump_pwm(200);
        d.update_warmwater(now, 65.0);
        d.update_warmwater(now + 1.0, 60.0);
        d.update_warmwater(now + 2.0, 50.0);
        assert!(d.check().is_some());

        d.update_warmwater(now + 10.0, 65.0);
        d.update_warmwater(now + 11.0, 60.0);
        d.update_warmwater(now + 12.0, 50.0);
        assert!(d.check().is_none()); // cooldown (10s < 120s)
    }

    #[test]
    fn alert_again_after_cooldown() {
        let mut d = AirInPipeDetector::new(10.0, 5.0, 50.0, 5.0);
        let now = 1000.0;
        d.update_pump_pwm(200);
        d.update_warmwater(now, 65.0);
        d.update_warmwater(now + 1.0, 60.0);
        d.update_warmwater(now + 2.0, 50.0);
        assert!(d.check().is_some());

        d.update_warmwater(now + 10.0, 65.0);
        d.update_warmwater(now + 11.0, 60.0);
        d.update_warmwater(now + 12.0, 50.0);
        assert!(d.check().is_some()); // cooldown expired (10s > 5s)
    }

    /// Python parity: BYTE2TEMP over the FULL u16 HC_STATE4 field
    /// (0x012C → (300*100)//255 = 117). The old &0xFF mask gave 17.
    #[test]
    fn hc_state4_byte2temp_full_u16() {
        assert_eq!(byte2temp_u16(0x012C), 117.0);
    }
}
