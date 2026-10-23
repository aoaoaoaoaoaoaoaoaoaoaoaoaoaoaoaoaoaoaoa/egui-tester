use xkeysym::Keysym;

use crate::{Error, Key, Result};

impl Key {
    pub(super) fn keysym(self) -> Result<Keysym> {
        Ok(match self {
            Self::Character(character) => {
                if u32::from(character) > 0xff {
                    return Err(Error::Unsupported {
                        capability: "text input",
                        detail: format!(
                            "`{character}` is outside the MVP's Latin-1 X11 key injection"
                        ),
                    });
                }
                Keysym::new(u32::from(character))
            }
            Self::Space => Keysym::new(u32::from(' ')),
            Self::Return => Keysym::Return,
            Self::Escape => Keysym::Escape,
            Self::Tab => Keysym::Tab,
            Self::Backspace => Keysym::BackSpace,
            Self::Delete => Keysym::Delete,
            Self::Home => Keysym::Home,
            Self::End => Keysym::End,
            Self::Left => Keysym::Left,
            Self::Right => Keysym::Right,
            Self::Up => Keysym::Up,
            Self::Down => Keysym::Down,
            Self::Function(number) => match number {
                1 => Keysym::F1,
                2 => Keysym::F2,
                3 => Keysym::F3,
                4 => Keysym::F4,
                5 => Keysym::F5,
                6 => Keysym::F6,
                7 => Keysym::F7,
                8 => Keysym::F8,
                9 => Keysym::F9,
                10 => Keysym::F10,
                11 => Keysym::F11,
                12 => Keysym::F12,
                _ => {
                    return Err(Error::Unsupported {
                        capability: "function key",
                        detail: format!("F{number} is outside the portable F1–F12 set"),
                    });
                }
            },
            Self::Shift => Keysym::Shift_L,
            Self::Control => Keysym::Control_L,
            Self::Alt => Keysym::Alt_L,
            Self::Super => Keysym::Super_L,
        })
    }
}
