use std::fmt;

pub struct Unit {
    value: usize,
    unit: String,
}

impl Unit {
    pub fn new(value: usize, unit: impl Into<String>) -> Self {
        Self {
            value,
            unit: unit.into(),
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut value = self.value;
        let mut prefix = UnitPrefix::None;

        while value > 10000 {
            value /= 1000;
            prefix = prefix.next();
        }

        write!(f, "{} {}{}", value, prefix, self.unit)
    }
}

enum UnitPrefix {
    None,
    Kilo,
    Mega,
    Giga,
    Tera,
    Peta,
    Exa,
    Impossible,
}

impl UnitPrefix {
    fn next(self) -> Self {
        match self {
            UnitPrefix::None => UnitPrefix::Kilo,
            UnitPrefix::Kilo => UnitPrefix::Mega,
            UnitPrefix::Mega => UnitPrefix::Giga,
            UnitPrefix::Giga => UnitPrefix::Tera,
            UnitPrefix::Tera => UnitPrefix::Peta,
            UnitPrefix::Peta => UnitPrefix::Exa,
            UnitPrefix::Exa | UnitPrefix::Impossible => UnitPrefix::Impossible,
        }
    }
}

impl fmt::Display for UnitPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self {
            UnitPrefix::None => "",
            UnitPrefix::Kilo => "k",
            UnitPrefix::Mega => "M",
            UnitPrefix::Giga => "G",
            UnitPrefix::Tera => "T",
            UnitPrefix::Peta => "P",
            UnitPrefix::Exa => "E",
            UnitPrefix::Impossible => "x",
        };
        f.write_str(prefix)
    }
}
