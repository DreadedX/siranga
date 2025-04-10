use tracing::trace;

#[derive(Debug)]
pub enum Input {
    Char(char),
    Up,
    Down,
    Esc,
    Enter,
    CtrlP,
    Other,
}

impl From<&[u8]> for Input {
    fn from(value: &[u8]) -> Self {
        match value {
            [c] if c.is_ascii_graphic() => Input::Char(*c as char),
            [27] => Input::Esc,
            [27, 91, 65] => Input::Up,
            [27, 91, 66] => Input::Down,
            [13] => Input::Enter,
            // NOTE: Actual char is DLE, this happens to map to ctrl-p
            [16] => Input::CtrlP,
            other => {
                trace!("{other:?}");
                Input::Other
            }
        }
    }
}
