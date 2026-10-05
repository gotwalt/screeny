# Changelog

## 0.1.1

- Each panel's Home Assistant device is named after the panel, and shows the
  panel's own firmware version. The firmware update entity no longer shows
  Unknown.
- A device left behind by another Studio on the same broker is cleared.
- Settings can be exported from one Studio and imported into another.
- The Studio carries firmware 0.11.0 and offers it to older panels.

## 0.1.0

- First version of the app: the Studio behind Home Assistant's ingress, with
  its MQTT broker taken from Home Assistant.
