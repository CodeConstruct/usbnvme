# Changelog

## 0.3.0 - 2025-07-31

### Added

- Added a PLDM file requester sequence. This runs on Set Endpoint ID,
  requesting a file from the first File Descriptor PDR.

### Changed

- Program now runs using DTCM memory for better performance.
  This requires an updated `xspiloader` bootloader to be programmed,
  running `cargo run --release` in the xspiloader directory (a one-off step).
  To revert to an older usbnvme version, the previous xspiloader will
  need to be programmed.

- Improved USB transmit performance (tested with mctp-bench).

- Now uses mctp-bench "receive request" protocol. An updated mctp-bench
  command line binary should be used, eg `mctp-bench eid 8 len 987 count 200000`.
  `mctp-bench` usbnvme feature will now remain idle until it receives a request.
  (mctp-bench https://github.com/CodeConstruct/mctp/pull/100)

- Updated nvme-mi-dev, adding NVMe MI configuration commands

- Increased RTT log buffer (probe-rs ST-Link logs), previously some logs would
  be lost during busy output.

- Moved some RAM sections for more space, rodata is now in SRAM3. Added optional
  stack usage logging. RAM layout now will catch stack overflows and fault.
  (Should not have any user-visible effect).

## 0.2.0 - 2025-06-25

### Added

- Add a NVMe-MI responder

### Changed

- Using published mctp crates

## 0.1.0 - 2025-05-21

Initial release
