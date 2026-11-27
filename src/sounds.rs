use serde::{Deserialize, Deserializer, Serialize};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SoundAction {
    SelectFile,
    SwitchTab,
}

impl SoundAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SelectFile => crate::i18n::tr("Seleccionar un archivo", "Select a file"),
            Self::SwitchTab => crate::i18n::tr("Abrir o cambiar de pestaña", "Open or switch tabs"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionSound {
    Plop,
    Pulso,
    GirlCat,
}

impl ActionSound {
    pub(crate) const PRESETS: [Self; 3] = [Self::Plop, Self::Pulso, Self::GirlCat];

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Plop => "Plop",
            Self::Pulso => "Pulso",
            Self::GirlCat => "Girl Cat",
        }
    }

    pub(crate) fn play(&self) -> bool {
        let flags = SND_ASYNC | SND_NODEFAULT;
        let bytes: &'static [u8] = match self {
            Self::Plop => include_bytes!("../assets/sounds/plop.wav"),
            Self::Pulso => include_bytes!("../assets/sounds/pulso.wav"),
            Self::GirlCat => include_bytes!("../assets/sounds/girl-cat.wav"),
        };
        // Los WAV integrados son estáticos y siguen disponibles para WinMM
        // hasta que acaba la reproducción asíncrona.
        unsafe { PlaySoundW(PCWSTR(bytes.as_ptr().cast()), None, flags | SND_MEMORY) }.as_bool()
    }
}

pub(crate) fn deserialize_assignment<'de, D: Deserializer<'de>>(de: D) -> Result<Option<ActionSound>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SavedSound {
        Preset(ActionSound),
        Unsupported(serde::de::IgnoredAny),
    }
    // Las asignaciones WAV de versiones anteriores pasan a Silencio.
    Ok(match Option::<SavedSound>::deserialize(de)? {
        Some(SavedSound::Preset(sound)) => Some(sound),
        _ => None,
    })
}
