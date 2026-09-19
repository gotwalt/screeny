# Bench notes

## Device

Tidbyt Gen 1: ESP32-D0WD-V3 rev 3.0, dual core 240 MHz, 8 MB flash, 40 MHz crystal,
MAC b4:8a:0a:4a:00:a4, CP2102N on `/dev/cu.usbserial-2140`. Stock firmware strings
mention `tidbyt/atca` (an ATECC secure element on I2C), `tidbyt/ble`, a button, and
ESP-IDF. Partition table and restore instructions: `backup/README.md`.

Serial link corrupts long transfers at 460800 baud and above. Use 230400.
Auto-reset into the bootloader over DTR/RTS works; no button press needed.

## Camera

Anker PowerConf C200, avfoundation. The Claude desktop app cannot get macOS camera
permission (it never appears in Privacy > Camera), so captures go through
`tools/cam-daemon.sh` running in Terminal.app (started with
`open -a Terminal tools/cam-daemon.sh`). Request a capture with:

    echo snap     > captures/req/NAME.req    # -> captures/NAME.jpg
    echo "clip 3" > captures/req/NAME.req    # -> captures/NAME.mp4
    # wait until the .req file disappears; errors land in captures/NAME.err

ffmpeg quirks: the camera advertises exactly 30.000030 fps and `-framerate 30` fails
with "Could not lock device for configuration"; pixel format must be one of
uyvy422/yuyv422/nv12; pass `-nostdin`. Modes: 320x240 up to 2560x1440, all 30 fps.

In the 1920x1080 frame the panel spans roughly x 570-1420, y 380-770, viewed from
slightly above (mild keystone). The room is daylit and the camera auto-exposes for
the room, so lit LEDs clip to near-white: colour accuracy checks will need exposure
control (UVC) or a darker scene. The glossy table mirrors the panel below it; crop
it out before any image analysis.

## Stock word clock (reference for the word-clock sender)

![stock word clock](img/stock-word-clock.jpg)

At 11:43 the stock app shows three lines, "QUARTER / TILL / TWELVE", upper case,
blue-white on black, a ~5x7 pixel font with 1 px spacing, each line indented a few
pixels further than the last (a staircase), top-left anchored with a ~3 px margin.
Time is rounded to the nearest five minutes and phrased colloquially.
