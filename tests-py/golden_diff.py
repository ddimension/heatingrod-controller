"""Golden-diff: Python aioesphomeserver vs Rust esphome-api-server.

Drives BOTH servers with the SAME production config and diffs the entity
streams a HA client (aioesphomeapi) receives. Any difference here is an
entity-registry change in HA — must be empty before cutover.

Run from the repo root with the heatingrod venv:
    rust/tests-py/golden_diff.py

Python server: heatingrod's own ESPHomeBridge (port 6056, 127.0.0.1).
Rust server:   rust/target/debug/heatingrod with a derived config (port 6057).
"""

import asyncio
import os
import subprocess
import sys
import threading
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)  # heatingrod package

from aioesphomeapi import APIClient  # noqa: E402

PY_PORT, RUST_PORT = 6056, 6057
PRODUCTION_CONFIG = os.path.join(REPO, "rust", "config", "production.yaml")


def normalize(ents):
    """(type, object_id, name, key, device_id, extras...) rows in stream order."""
    rows = []
    for row in ents:
        for e in row if isinstance(row, list) else [row]:
            t = type(e).__name__
            base = [
                t, e.object_id, e.name, e.key, e.device_id,
                getattr(e, "icon", ""),
                getattr(e, "device_class", ""),
            ]
            if t == "SensorInfo":
                rows.append(base + [
                    e.unit_of_measurement, e.accuracy_decimals,
                    int(e.state_class), e.force_update,
                ])
            elif t == "BinarySensorInfo":
                rows.append(base + [e.is_status_binary_sensor])
            elif t == "TextSensorInfo":
                rows.append(base + [])
            elif t == "NumberInfo":
                rows.append(base + [
                    e.min_value, e.max_value, e.step,
                    e.unit_of_measurement, int(e.mode),
                ])
            elif t == "ClimateInfo":
                rows.append(base + [
                    sorted([int(m) for m in e.supported_modes]),
                    e.visual_min_temperature, e.visual_max_temperature,
                    e.visual_target_temperature_step,
                    e.visual_current_temperature_step,
                    bool(e.supports_current_temperature),
                    bool(e.supports_action),
                ])
            else:
                rows.append(base + [repr(e)])
    return rows


async def dump(port, psk, label):
    c = APIClient("127.0.0.1", port, password=None, noise_psk=psk)
    await c.connect(login=True)
    info = await c.device_info()
    ents = await c.list_entities_services()
    rows = normalize(ents)
    print(f"[{label}] {info.name} mac={info.mac_address} "
          f"devices={[(d.device_id, d.name) for d in info.devices]} "
          f"entities={len(rows)}")
    await c.disconnect()
    return rows


async def run_python_server():
    from heatingrod.esphome_bridge import ESPHomeBridge

    config = load_config()
    config.api.port = PY_PORT
    bridge = ESPHomeBridge(config)
    bridge.setup()
    await bridge.start()
    try:
        await asyncio.sleep(3600)
    finally:
        await bridge.stop()


def load_config():
    os.environ.setdefault("HEATINGROD_CONFIG", PRODUCTION_CONFIG)
    from heatingrod.config import load_config as _load
    return _load()


def main():
    psk = load_config().api.encryption_key

    # Derived config for the Rust binary: same as production, different port
    # and MAC (the two servers must not collide).
    derived = PRODUCTION_CONFIG.replace(".yaml", "-6057.yaml")
    with open(PRODUCTION_CONFIG) as f:
        content = f.read().replace("port: 6053", "port: 6057").replace(
            "mac_address: B8:27:EB:96:AE:00",
            "mac_address: B8:27:EB:96:AE:01",
        )
    with open(derived, "w") as f:
        f.write(content)

    py_thread = threading.Thread(
        target=lambda: asyncio.run(run_python_server()), daemon=True)
    py_thread.start()
    rust_proc = subprocess.Popen(
        [os.path.join(REPO, "rust", "target", "debug", "heatingrod"),
         "--config", derived],
        cwd=REPO, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    try:
        time.sleep(6)  # let both servers come up
        py_rows = asyncio.run(dump(PY_PORT, psk, "python"))
        rust_rows = asyncio.run(dump(RUST_PORT, psk, "rust"))

        assert len(py_rows) == len(rust_rows), \
            f"entity count differs: python={len(py_rows)} rust={len(rust_rows)}"

        diffs = [
            (i, a, b)
            for i, (a, b) in enumerate(zip(py_rows, rust_rows))
            if a != b
        ]
        if diffs:
            print(f"\nGOLDEN-DIFF FAILED: {len(diffs)} differences")
            for i, a, b in diffs[:20]:
                print(f"  [{i}] python: {a}")
                print(f"  [{i}] rust:   {b}")
            sys.exit(1)
        print(f"\nGOLDEN-DIFF OK: {len(py_rows)} entities identical "
              "(order, fields, keys)")
    finally:
        rust_proc.terminate()
        os.unlink(derived)
        try:
            rust_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            rust_proc.kill()


if __name__ == "__main__":
    main()
