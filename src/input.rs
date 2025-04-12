use tracing::trace;

#[derive(Debug)]
pub enum Input {
    Char(char),
    Up,
    Down,
    Delete,
    Esc,
    Enter,
    Backspace,
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
            [27, 91, 51, 126] => Input::Delete,
            [13] => Input::Enter,
            // NOTE: Actual char is DLE, this happens to map to ctrl-p
            [16] => Input::CtrlP,
            [127] => Input::Backspace,
            other => {
                trace!("{other:?}");
                Input::Other
            }
        }
    }
}
