# Raspberry Pi server
Primarily this project written to cover my own needs, but you can find something useful for
yourself. Also it has the [React frontend](https://github.com/lem0nez/rpi-control) which
written mostly by AI.

## Cross-compilation on Debian-based systems
First you need to install the development libraries of ALSA, D-Bus, Udev and FLAC runtime
library.

```
# dpkg --add-architecture arm64
# apt update
# apt install libasound2-dev:arm64 libflac12:arm64 libdbus-1-dev:arm64 libudev-dev:arm64
```

Then you need to link the FLAC library (required by `flac-bound` crate,
see [documentation](https://docs.rs/flac-bound/0.3.0/flac_bound/index.html#building-)).
- Either locally in the project.
```
$ ln --symbolic \
     /usr/lib/aarch64-linux-gnu/libFLAC.so.12 \
     target/aarch64-unknown-linux-gnu/<BUILD_PROFILE>/deps/libflac.so
```
- Or system-wide.
```
# cd /usr/lib/aarch64-linux-gnu
# ln --symbolic libFLAC.so.12 libflac.so
```

After that you can build the binary.

```
$ export PKG_CONFIG_SYSROOT_DIR=/usr/aarch64-linux-gnu
$ cargo build --target aarch64-unknown-linux-gnu
```

## Running
To run the server you must set some required parameters. It can be achieved in two ways.
1. By settings environment variables with the `RPI_` prefix.
2. By putting values inside the `/etc/rpi-server.yaml` configuration file.

If the server started successfully, you can view logs using the following command:

```
$ journalctl --identifier rpi-server
```

### Configuration
Required parameters described with the `[REQUIRED]` keyword, others are optional.

```yaml
# /etc/rpi-server.yaml

# Address to bind the server to.
server_address: 0.0.0.0
# Port which used to bind the server.
server_port: 80
# Log level filter. Can be one of: OFF, ERROR, WARN, INFO, DEBUG or TRACE.
log_level: INFO
# [REQUIRED] Directory with read-only resources. It has the following structure:
#   site/ - directory with static files to host on "/"
assets_dir: /path/to/assets
# Directory where to store user preferences, database and other data.
data_dir: /var/lib/rpi-server
# If string is specified, requests to the server will require
# authentication with this Bearer Token.
access_token: null

# Bluetooth-related parameters.
bluetooth:
  # How long to perform the discovery.
  discovery_seconds: 5
  # Name of Bluetooth adapter to use for the devices discovering.
  adapter_name: null
  # [REQUIRED] MAC address of Xiaomi Mi Temperature and Humidity Monitor 2 (LYWSD03MMC).
  lounge_temp_mac_address: FF:00:FF:00:FF:00

# [OPTIONAL] Hotspot information.
# If this section is not null, all child parameters must be defined.
#
# Hotspot is a device that shares the internet using Wi-Fi. But the same device can connect to
# Raspberry Pi via Bluetooth, for example, to stream the audio. And if the same device will do these
# two operations simultaneously, stability of the audio streaming will be bad. So, we temporary
# disconnect from the Wi-Fi access point while the device connected to us via Bluetooth.
hotspot:
  # [REQUIRED] NetworkManager connection. Can be one of: ID (name), UUID or path.
  connection: AP
  # [REQUIRED] Bluetooth MAC address of the hotpost device.
  bluetooth_mac_address: FF:00:FF:00:FF:00

# Piano parameters.
piano:
  # [REQUIRED] Identifier of an audio device. You can find it in the /proc/asound/cards file.
  device_id: PIANO
  # ALSA plugin to use for audio input / output.
  # To list available plugins, run "arecord --list-pcms".
  alsa_plugin: plughw
```
