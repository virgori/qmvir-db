"""Test SO_REUSEADDR + graceful shutdown — rapid restart on same port."""
import qm_engine
import time

# Test 1: rapid restart on same port
gw = qm_engine.PostgresGateway(port=15432)
gw.start_native()
time.sleep(0.3)
assert gw.is_running, "Server did not start"
print("Start 1: OK, running =", gw.is_running)

gw.stop()
time.sleep(0.1)
print("Stop 1: OK, running =", gw.is_running)

# Restart immediately on same port -- would fail before SO_REUSEADDR
gw2 = qm_engine.PostgresGateway(port=15432)
gw2.start_native()
time.sleep(0.3)
assert gw2.is_running, "Second start failed -- SO_REUSEADDR not working"
print("Start 2: OK, running =", gw2.is_running)

gw2.stop()
time.sleep(0.1)
print("Stop 2: OK")

# Test 2: triple rapid restart
for i in range(3):
    g = qm_engine.PostgresGateway(port=15432)
    g.start_native()
    time.sleep(0.2)
    assert g.is_running, f"Restart {i} failed"
    g.stop()
    time.sleep(0.05)
    print(f"Rapid restart {i}: OK")

print("ALL PASS -- SO_REUSEADDR + graceful shutdown working")
