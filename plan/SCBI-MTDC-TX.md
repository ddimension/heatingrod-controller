# SCBI TX on the MTDCv5 — verified dead end (19.09.2026)

Goal: force the SOREL warm-water pump off via CAN (duty-cycle limiter).

## Evidence chain

1. Official spec present: `docs/Sorel/SOREL_CAN_bus_interface_[rev7] - CUSTOMERS.pdf`
   (REMOTERELAY §2.2.7, REMOTESENSOR §2.2.5, EVENT/EFT_EXFUNC, PARAM_MONITORING).
2. TX path implemented and proven: slcan `T` command; the USBtin acks every
   transmit with `z\r` (fischl.de/usbtin: `z` = transmit success; BEL = error).
   Probe logs show acks for every frame — frames DO reach the bus.
3. Probes (supervised, one-shot, 19.09.2026):
   - REMOTERELAY OFF→3s→ON (relay 0, peer id 22): no relay echo, no reaction
   - REMOTESENSOR requests (func 8 storage-top, func 3 solar), addressed with
     client=0x20 (sender) AND client=0x16 (peer id, acceptance-filter
     discriminator): no response in either addressing
   - MTDC menu has no remote-relay / CAN-control setting
4. Conclusion: the MTDCv5 firmware only SENDS telemetry (DATALOGGER/HCC/
   CONTROLLER I_AM_HERE); it does not implement SCBI request/response
   handling. CAN control is not possible on this device.

## Fallback (decided path)

Relay channel 2 of the kaskade ESP (HW-622, GPIO5 free): cut the SOREL's
230V supply (or only the pump). The duty-cycle limiter state machine
(canbus.rs SorelRelayLimiter) is actuator-agnostic — swap REMOTERELAY TX
for a kaskade switch command. HA controls (input_boolean.heizung_sorel_limit,
input_number.heizung_sorel_limit_minuten) already exist.
