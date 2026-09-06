"""Phase-0 cross-check: aioesphomeapi (the library HA uses) against the Rust spike."""
import asyncio

from aioesphomeapi import APIClient

HOST, PORT = "127.0.0.1", 6054
PSK = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="


async def main():
    c = APIClient(HOST, PORT, password=None, noise_psk=PSK)
    await c.connect(login=True)
    try:
        info = await c.device_info()
    except TypeError:
        info = c.device_info  # older aioesphomeapi
    print(f"name={info.name} mac={info.mac_address}")
    print(f"esphome_version={info.esphome_version} project={info.project_name}/{info.project_version}")
    print(f"devices={[(d.device_id, d.name) for d in info.devices]}")
    ents = await c.list_entities_services()
    for row in ents:
        print(f"  {row!r}")

    def cb(state):
        print(f"  state: {state}")

    c.subscribe_states(cb)
    await asyncio.sleep(11)  # initial burst + two pushes

    print("  -> climate_command(mode=3, target=72)")
    c.climate_command(0x23456789, mode=3, target_temperature=72.0)
    await asyncio.sleep(2)
    await c.disconnect()


async def wrong_key():
    # valid base64, 32 bytes, but not OUR key -> server-side MAC failure path
    import base64

    wrong = base64.b64encode(b"B" * 32).decode()
    c = APIClient(HOST, PORT, password=None, noise_psk=wrong)
    try:
        await c.connect(login=True)
        print("wrong-key: UNEXPECTED connect success")
    except Exception as e:
        print(f"wrong-key rejected: {type(e).__name__}: {str(e)[:100]}")


async def plaintext_rejected():
    c = APIClient(HOST, PORT, password=None)
    try:
        await c.connect(login=True)
        print("plaintext: UNEXPECTED connect success")
    except Exception as e:
        print(f"plaintext rejected: {type(e).__name__}: {str(e)[:100]}")


async def run_all():
    await asyncio.gather(main(), wrong_key(), plaintext_rejected())


asyncio.run(run_all())
