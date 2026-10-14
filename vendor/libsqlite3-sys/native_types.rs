//! One registry shared by the SQLite build and native value implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Decimal = 1,
    Uuid = 2,
    Instant = 3,
    Date = 4,
    DateTime = 5,
    Interval = 6,
}

#[allow(dead_code)] // Names/affinities are consumed by the build, payloads by Rust.
pub struct Descriptor {
    pub kind: Kind,
    pub name: &'static str,
    pub affinity: u8,
    pub tag: u8,
    pub payload_size: usize,
}

pub const TYPES: &[Descriptor] = &[
    Descriptor {
        kind: Kind::Decimal,
        name: "DECIMAL",
        affinity: 0x47,
        tag: b'd',
        payload_size: 0,
    },
    Descriptor {
        kind: Kind::Uuid,
        name: "UUID",
        affinity: 0x48,
        tag: b'u',
        payload_size: 16,
    },
    Descriptor {
        kind: Kind::Instant,
        name: "INSTANT",
        affinity: 0x49,
        tag: b'i',
        payload_size: 12,
    },
    Descriptor {
        kind: Kind::Date,
        name: "DATE",
        affinity: 0x4a,
        tag: b'a',
        payload_size: 4,
    },
    Descriptor {
        kind: Kind::DateTime,
        name: "DATETIME",
        affinity: 0x4b,
        tag: b't',
        payload_size: 12,
    },
    Descriptor {
        kind: Kind::Interval,
        name: "INTERVAL",
        affinity: 0x4c,
        tag: b'v',
        payload_size: 16,
    },
];
