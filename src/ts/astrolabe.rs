use astrolabe::{Date, DateTime, Time};

use super::{TS, impl_primitives};

impl_primitives!(Date, DateTime, Time => "string");
