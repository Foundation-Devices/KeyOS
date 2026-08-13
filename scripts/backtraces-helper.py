# SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
# SPDX-License-Identifier: GPL-3.0-or-later

import time

STOP_POINTS = ("::main", "::swi_handler")
BIN_DIR = "target/armv7a-unknown-xous-elf/release"

class CleanBadUnwinds:
    def __init__(self):
        self.name = "fixbt"
        self.enabled = True
        self.priority = 100

    def filter(self, frames):
        for frame in frames:
            yield frame
            function = frame.function()
            if any(function.endswith(stop) for stop in STOP_POINTS):
                return

gdb.frame_filters["fixbt"] = CleanBadUnwinds()

gdb.set_parameter("pagination", "off")
gdb.execute(f"add-symbol-file {BIN_DIR}/keyos-kernel")
gdb.execute("target remote :3334")

ss = gdb.lookup_global_symbol("keyos_kernel::services::SYSTEM_SERVICES")

def find_process(name):
    processes = ss.value()["processes"]
    lowest, highest = processes.type.range()
    for index in range(lowest, highest + 1):
        try:
            process = processes[index]["Some"]["__0"]
            if process["name"]["Some"]["__0"].string("utf-8") == name:
                return index + 1, int(process["aslr_slide"])
        except Exception:
            continue
    return None

def wait_for_process(name):
    while True:
        gdb.execute("monitor halt")
        found = find_process(name)
        gdb.execute("monitor go")
        if found is not None:
            return found
        print(f"Waiting for {name} to start")
        time.sleep(0.5)

target_pid, aslr_slide = wait_for_process(process)
# The binaries are PIE, so the slide is only known once the kernel has loaded the process.
gdb.execute(f"add-symbol-file {BIN_DIR}/{process} -o {aslr_slide:#x}")
print(f"Tracing {process}: pid={target_pid}, ASLR slide={aslr_slide:#x}")

while True:
    gdb.execute("monitor halt")
    cp15_raw_output = gdb.execute("monitor cp15 13 0 0 1", to_string=True)
    current_pid = int(cp15_raw_output.split("0x")[1].split(")")[0], 16) & 0xff
    if current_pid == target_pid:
        gdb.execute("maint flush register-cache")
        gdb.execute("bt 100")
        print(" --- ")
    gdb.execute("monitor go")
    time.sleep(0.05)
