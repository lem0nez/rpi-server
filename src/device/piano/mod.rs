use std::{
    ffi::OsString,
    fs::File,
    io::BufWriter,
    sync::{mpsc::channel, Arc},
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{error, info, warn};

use crate::{bluetooth::A2DPSourceHandler, config, SharedRwLock};

/// Delay between initializing just plugged in piano and finding its audio device.
///
/// Why it's required?
/// There is the only way to access the required audio device using [cpal]: iterating over all
/// available devices and picking the required one. When iterating over devices, they are become
/// busy. In this short period when the piano just plugged in, system's sound server needs a device
/// to be available to perform the initialization stuff. But if the device is busy,
/// it will not be picked up.
const FIND_AUDIO_DEVICE_DELAY: Duration = Duration::from_millis(500);

pub enum HandledPianoEvent {
    Add,
    Remove,
}

pub struct UpdateAudioDeviceParams {
    /// Whether calling the update just after the piano initialized.
    pub after_piano_init: bool,
}

#[derive(Clone)]
pub struct Piano {
    config: config::Piano,
    /// Used to check whether an audio device is in use by a Bluetooth device.
    a2dp_source_handler: A2DPSourceHandler,
    /// If the piano is not connected, it will be [None].
    inner: SharedRwLock<Option<InnerInitialized>>,
}

impl Piano {
    pub fn new(config: config::Piano, a2dp_source_handler: A2DPSourceHandler) -> Self {
        Self {
            config,
            a2dp_source_handler,
            inner: Arc::default(),
        }
    }

    pub async fn handle_udev_event(&self, event: &tokio_udev::Event) -> Option<HandledPianoEvent> {
        if !event
            .subsystem()
            .map(|subsystem| subsystem == "sound")
            .unwrap_or(false)
        {
            return None;
        }

        let event_type = event.event_type();
        if event_type == tokio_udev::EventType::Add {
            let id_matches = event
                .attribute_value("id")
                .map(|id| id.to_string_lossy() == self.config.device_id)
                .unwrap_or(false);

            if id_matches {
                if event.is_initialized() {
                    self.init_if_not_done(event.devpath().to_os_string()).await;
                    return Some(HandledPianoEvent::Add);
                } else {
                    error!("Udev device found, but it's not initialized");
                }
            }
        } else if event_type == tokio_udev::EventType::Remove {
            let devpath_matches = self
                .inner
                .read()
                .await
                .as_ref()
                .map(|inner| event.devpath() == inner.devpath)
                .unwrap_or(false);

            if devpath_matches {
                *self.inner.write().await = None;
                info!("Piano removed");
                return Some(HandledPianoEvent::Remove);
            }
        }
        None
    }

    pub async fn init_if_not_done(&self, devpath: OsString) {
        let mut inner = self.inner.write().await;
        if inner.is_none() {
            *inner = Some(InnerInitialized {
                devpath,
                device: None,
            });
            drop(inner);
            info!("Piano initilized");
            self.update_audio_device_if_applicable(UpdateAudioDeviceParams {
                after_piano_init: true,
            })
            .await;
        } else {
            warn!("Initialization skipped, because it's already done");
        }
    }

    /// If the piano initialized, sets or releases the audio device,
    /// according to if there is an connected A2DP source.
    pub async fn update_audio_device_if_applicable(&self, params: UpdateAudioDeviceParams) {
        if let Some(inner) = self.inner.write().await.as_mut() {
            if self.a2dp_source_handler.has_connected().await {
                if inner.device.is_some() {
                    inner.device = None;
                    info!("Audio device released");
                }
            } else if inner.device.is_none() {
                let self_clone = self.clone();
                tokio::spawn(async move {
                    if params.after_piano_init {
                        info!("Waiting before finding an audio device...");
                        tokio::time::sleep(FIND_AUDIO_DEVICE_DELAY).await;
                    }
                    if let Some(inner) = self_clone.inner.write().await.as_mut() {
                        // It can be changed while waiting.
                        if inner.device.is_some() {
                            return;
                        }
                        inner.device = self_clone.find_audio_device();
                        if inner.device.is_some() {
                            info!("Audio device set");
                            debug(inner.device.clone().unwrap()).await;
                        } else {
                            error!("Audio device is not found");
                        }
                    }
                });
            }
        }
    }

    pub fn find_devpath(&self) -> Option<OsString> {
        match tokio_udev::Enumerator::new() {
            Ok(mut enumerator) => {
                let match_result = enumerator
                    .match_subsystem("sound")
                    .and_then(|_| enumerator.match_is_initialized())
                    .and_then(|_| enumerator.match_attribute("id", &self.config.device_id));

                if let Err(e) = match_result {
                    error!("Failed to apply filters to the udev piano scanner: {e}");
                } else {
                    match enumerator.scan_devices() {
                        Ok(mut devices) => {
                            return devices.next().map(|device| device.devpath().to_os_string());
                        }
                        Err(e) => error!("Failed to scan /sys for the piano: {e}"),
                    }
                }
            }
            Err(e) => error!("Failed to set up the udev piano scanner: {e}"),
        }
        None
    }

    fn find_audio_device(&self) -> Option<cpal::Device> {
        match cpal::default_host().devices() {
            Ok(devices) => {
                for device in devices {
                    match device.name() {
                        Ok(name) => {
                            if name.starts_with(&format!(
                                "{}:CARD={}",
                                self.config.alsa_plugin, self.config.device_id
                            )) {
                                return Some(device);
                            }
                        }
                        Err(e) => error!("Failed to get an audio device name: {e}"),
                    }
                }
            }
            Err(e) => error!("Failed to list the audio devices: {e}"),
        }
        None
    }
}

struct InnerInitialized {
    devpath: OsString,
    /// Will be [None] if the audio device is in use now.
    device: Option<cpal::Device>,
}

async fn debug(device: cpal::Device) {
    let device_clone = device.clone();
    let (record_tx, record_rx) = channel();

    let device_config = device_clone.default_input_config().unwrap();

    let wav_spec = hound::WavSpec {
        channels: device_config.channels(),
        sample_rate: device_config.sample_rate().0 as _,
        bits_per_sample: (device_config.sample_format().sample_size() * 8) as _,
        sample_format: if device_config.sample_format().is_float() {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let wav_writer = Arc::new(std::sync::Mutex::new(Some(
        hound::WavWriter::create("/tmp/piano-record.wav", wav_spec).unwrap(),
    )));
    let wav_writer_clone = wav_writer.clone();

    let err_fn = |err| {
        error!("an error occurred on stream: {}", err);
    };

    std::thread::spawn(move || {
        let stream = match device_config.sample_format() {
            cpal::SampleFormat::I8 => device_clone
                .build_input_stream(
                    &device_config.into(),
                    move |data, _| write_input_data::<i8>(data, &wav_writer_clone),
                    err_fn,
                    None,
                )
                .unwrap(),
            cpal::SampleFormat::I16 => device_clone
                .build_input_stream(
                    &device_config.into(),
                    move |data, _| write_input_data::<i16>(data, &wav_writer_clone),
                    err_fn,
                    None,
                )
                .unwrap(),
            cpal::SampleFormat::I32 => device_clone
                .build_input_stream(
                    &device_config.into(),
                    move |data, _| write_input_data::<i32>(data, &wav_writer_clone),
                    err_fn,
                    None,
                )
                .unwrap(),
            cpal::SampleFormat::F32 => device_clone
                .build_input_stream(
                    &device_config.into(),
                    move |data, _| write_input_data::<f32>(data, &wav_writer_clone),
                    err_fn,
                    None,
                )
                .unwrap(),
            _ => panic!("unsupported sample format"),
        };

        error!("Recording...");
        stream.play().unwrap();
        record_rx.recv().unwrap();
        drop(stream);
        wav_writer
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .finalize()
            .unwrap();
        error!("Recorded");
    });

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        record_tx.send(()).unwrap();
    });
}

type WavWriterHandle = Arc<std::sync::Mutex<Option<hound::WavWriter<BufWriter<File>>>>>;

fn write_input_data<T>(input: &[T], writer: &WavWriterHandle)
where
    T: cpal::Sample + hound::Sample,
{
    if let Ok(mut guard) = writer.try_lock() {
        if let Some(writer) = guard.as_mut() {
            for &sample in input.iter() {
                let sample = T::from_sample(sample);
                writer.write_sample(sample).ok();
            }
        }
    }
}
