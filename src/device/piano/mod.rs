use std::{
    ffi::OsString,
    fs::File,
    io::BufWriter,
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, SampleRate,
};
use flac_bound::FlacEncoder;
use log::{error, info, warn};
use midir::{MidiInput, MidiOutput};
use rodio::Sink;
use tokio::sync::mpsc::channel;

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
            error!("{}", event.syspath().to_string_lossy());
            if event.syspath().to_string_lossy().ends_with("/midi2") {
                // debug_midi().await;
                return None;
            }

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
    let (record_tx, mut record_rx) = channel::<Vec<i32>>(128);

    let device_config = device_clone
        .supported_input_configs()
        .unwrap()
        .find(|config| config.channels() == 2 && config.sample_format() == SampleFormat::I32)
        .unwrap()
        .try_with_sample_rate(SampleRate(44_100))
        .unwrap();

    tokio::task::spawn_blocking(move || {
        let mut outf = File::create("/tmp/piano-record.flac").unwrap();
        let mut wrap = flac_bound::WriteWrapper(&mut outf);
        let mut encoder = FlacEncoder::new()
            .unwrap()
            .channels(device_config.channels() as _)
            .bits_per_sample((device_config.sample_format().sample_size() * 8) as _)
            .sample_rate(device_config.sample_rate().0)
            .init_write(&mut wrap)
            .unwrap();

        let stream = device_clone
            .build_input_stream(
                &device_config.clone().into(),
                move |data: &[i32], _| {
                    record_tx.blocking_send(data.to_vec()).unwrap();
                },
                |err| error!("an error occurred on stream: {}", err),
                None,
            )
            .unwrap();

        error!("Recording...");
        stream.play().unwrap();
        let instant = Instant::now();

        while let Some(data) = record_rx.blocking_recv() {
            if instant.elapsed() > Duration::from_secs(3) {
                encoder.finish().unwrap();
                drop(stream);
                break;
            } else {
                encoder.process_interleaved(&data, (data.len() / 2) as _).unwrap();
            }
        }
        error!("Recorded");
    });

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        // record_tx.send(()).unwrap();
    });

    let device_config = device.default_output_config().unwrap();
    let shared_sink: SharedRwLock<Option<Sink>> = SharedRwLock::default();
    let shared_sink_clone = Arc::clone(&shared_sink);
    tokio::task::spawn_blocking(move || {
        let (_stream, handle) =
            rodio::OutputStream::try_from_device_config(&device, device_config).unwrap();
        let sink = rodio::Sink::try_new(&handle).unwrap();

        sink.append(
            rodio::Decoder::new(std::io::BufReader::new(
                std::fs::File::open("/tmp/piano-song.flac").unwrap(),
            ))
            .unwrap(),
        );
        *shared_sink_clone.blocking_write() = Some(sink);
        error!("PLAYING");
        shared_sink_clone
            .blocking_read()
            .as_ref()
            .unwrap()
            .sleep_until_end();
        error!("PLAY STOPPED");
    });

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        shared_sink.read().await.as_ref().unwrap().stop();
        error!("STOP SENT");
    });
}

type WavWriterHandle = Arc<std::sync::Mutex<Option<hound::WavWriter<BufWriter<File>>>>>;

fn write_input_data<T>(input: &[T], writer: &WavWriterHandle)
where
    T: cpal::Sample<Float = f32> + hound::Sample,
{
    if let Ok(mut guard) = writer.try_lock() {
        if let Some(writer) = guard.as_mut() {
            for &sample in input.iter() {
                let sample = T::from_sample(sample);
                writer.write_sample(sample.mul_amp(10.0)).ok();
            }
        }
    }
}

async fn debug_midi() {
    let midi_in = MidiInput::new("client_name").unwrap();
    for port in midi_in.ports() {
        error!("PORT: {}", midi_in.port_name(&port).unwrap());
    }
    let midi_in_port = midi_in
        .ports()
        .into_iter()
        .find(|port| {
            midi_in
                .port_name(port)
                .map(|name| name.contains("SOUND"))
                .unwrap_or(false)
        })
        .unwrap();

    let midi_out = MidiOutput::new("client_name_2").unwrap();
    let midi_out_port = midi_out
        .ports()
        .into_iter()
        .find(|port| {
            midi_out
                .port_name(port)
                .map(|name| name.contains("SOUND"))
                .unwrap_or(false)
        })
        .unwrap();

    let in_connection = midi_in
        .connect(
            &midi_in_port,
            "in_port_name",
            move |stamp, msg, _| error!("MIDI_IN {}: {:?} (len = {})", stamp, msg, msg.len()),
            (),
        )
        .unwrap();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        in_connection.close();
        error!("MIDI IN STOP");
    });

    let mut out_connection = midi_out.connect(&midi_out_port, "out_port_name").unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;

        let mut play_note = |note: u8, duration: u64| {
            const NOTE_ON_MSG: u8 = 0x90;
            const NOTE_OFF_MSG: u8 = 0x80;
            const VELOCITY: u8 = 0x64;
            out_connection.send(&[NOTE_ON_MSG, note, VELOCITY]).unwrap();
            error!("MIDI SEND");
            std::thread::sleep(Duration::from_millis(duration));
            out_connection
                .send(&[NOTE_OFF_MSG, note, VELOCITY])
                .unwrap();
        };

        play_note(36, 50);
        play_note(36, 50);
        play_note(36, 50);
        play_note(36, 50);
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
}
