---
title: Hillside weather station
status: draft
tags: [hardware, firmware, garden]
---

# Hillside weather station

A solar-powered weather station for a garden shed: a small board wakes up, reads its sensors,
sends them over LoRa to a receiver in the house, and goes back to sleep. Parts and wiring are in
[Hardware](hardware.md), what is measured and how in [Sensors](sensors.md), the formula in
[its own section](sensors.md#dew-point). What comes next is in the [Roadmap](roadmap.md). The
[calibration notes](calibration.md) are not written yet, so their link shows in red.

> [!NOTE]
> Every figure here comes from one station on one hill. Yours will differ.

| File | What |
|------|------|
| [`data/readings.csv`](data/readings.csv) | a morning of readings |
| [`data/station.yaml`](data/station.yaml) | the station's settings |
| [`data/sensors.json`](data/sensors.json) | the sensors and their buses |
| [`firmware/dewpoint.rs`](firmware/dewpoint.rs) | the dew point, as the board computes it |

The receiver keeps a week of readings and serves them on the home network.[^1]

## Building it

1. Order the parts listed in [Hardware](hardware.md#parts).
2. Flash the firmware (see [Flashing](hardware.md#flashing)).
3. Mount the box at least 1.5 m above the ground, away from walls.

[^1]: Nothing leaves the house: there is no cloud service to sign up for.
