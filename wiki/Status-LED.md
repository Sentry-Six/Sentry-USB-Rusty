# Status light

**Two short flashes, then a pause = connected and ready.** This repeats every
three seconds. The light stays off between the two-flash groups.

These patterns start after Linux boots and Sentry USB takes over the Pi's light.
**Colours are for Pi 5 only.** On Pi 4, Pi Zero 2 W and Pi 3A+, read the same
patterns on the green activity light. Any separate power light stays unchanged.

| What you see | What it means | What to do |
| --- | --- | --- |
| **Two short flashes**, then a pause | **Connected** to the car or another USB host | Normal. No action needed. |
| **Fast pulses, mostly lit**, with brief dark gaps | **Storage writes detected** | Normal during write activity. Returns to two flashes after about three quiet seconds. |
| **One short flash**, then a pause | **Waiting for USB**, or the USB connection is suspended | If the car is awake, check its USB connection. |
| **Slow, even blinking** | **Starting** Sentry USB | Wait for startup to finish. |
| **Fast, even blinking** | **Preparing** storage or doing USB maintenance | Wait for the operation to finish. |
| **Mostly lit**, with one brief dark dip every three seconds | **Needs attention** | Open the Sentry USB web UI and check its status and logs. |

On **Pi 5**: starting/preparing is **amber**, connected/waiting/writing is
**green**, and needs attention is **red**. Amber means both colours lit together.

**Writing is not proof of saved video.** The light detects changes to camera
storage; formatting and other disk writes can trigger it too. Check the car's
recording indicator or saved clips to confirm footage.

<p align="center">
  <a href="images/status-led-guide.svg"><img src="images/status-led-guide.svg" alt="Animated examples: connected is two short flashes; storage writes are fast pulses with brief dark gaps; waiting is one flash; starting is slow even blinking; preparing is fast even blinking; attention is one dark dip every three seconds. Pi 5 colours and other Pis' green lights are shown side by side." width="640"></a><br>
  <em>Tap the image to open the full-size animated guide.</em>
</p>

<details>
<summary><strong>Pi 5: red/green alternating, or two dark dips instead of flashes?</strong></summary>

Some versions of the LED controller display the Pi 5's green light backwards:

| What you see | What it may actually mean |
| --- | --- |
| Red and green alternate slowly | Starting; intended to blink amber |
| Red and green alternate quickly | Preparing; intended to blink amber |
| Green most of the time, with two short dark dips | Connected; intended to flash green twice |

This is a known Pi 5 polarity issue. **Alternating red/green is not an extra
Sentry USB status.** Check whether your installed release includes the Pi 5 LED
polarity fix. If reporting this behaviour, include your Pi model, Sentry USB
version, kernel version and whether the car is connected.

An unknown board or software version cannot be diagnosed from its colours alone.
The fix reads the effective green-light polarity and respects an existing
`act_led_activelow=off` setting; it does not change boot configuration. Unsupported
or remapped GPIO descriptions leave application LED control unavailable and log
the reason. Other Pi models keep their normal brightness mapping.

[Raspberry Pi explains why the Pi 5 green channel is inverted.](https://github.com/raspberrypi/linux/issues/6783)

</details>

<details>
<summary><strong>No Sentry USB pattern, or stuck on starting/preparing?</strong></summary>

During boot, the Pi still uses its own LED signals. See
[Raspberry Pi's boot-failure codes](https://www.raspberrypi.com/documentation/computers/configuration.html#led-behaviour)
if it does not finish booting.

Once booted, check the web UI and logs if startup or maintenance does not finish.
During an upgrade, LED control waits for an older archive worker to exit. Use the
normal update/reboot flow to complete that handoff.

The light alone cannot diagnose a frozen process or a power failure. Missing or
unsupported LED hardware also does not stop Sentry USB from working.

</details>

<details>
<summary><strong>Exact timings and technical limits</strong></summary>

| State | On/off timing |
| --- | --- |
| Connected | 100 ms on, 150 ms off, 100 ms on, 2,650 ms off |
| Storage writes | 400 ms on, 100 ms off |
| Waiting | 100 ms on, 2,900 ms off |
| Starting | 1 second on, 1 second off |
| Preparing | 250 ms on, 250 ms off |
| Needs attention | 2,800 ms on, 200 ms off |

These are on/off flashes, not smooth fading. On a single-colour Pi, preparing
spends equal time lit and dark; writing stays lit longer, with brief dark gaps.

- **Connected** means normal server startup completed, the archive worker is
  alive, storage is usable and the USB host configured the drive. It does not
  certify BLE, cloud/archive transfers or video contents.
- **Writing** watches new changes to the writable camera image while USB is
  connected. A new observer, image replacement, reconnect or USB maintenance
  resets the baseline. Old timestamps do not count as new writes. Archive phase
  changes alone do not clear the three-second write hold. BLE's separate
  awake/asleep heuristic is unchanged.
- USB maintenance is tracked for the whole gadget-cycle lock. USB state is
  sampled every 500 ms, so a physical disconnect and reconnect between samples
  can be missed. Software-controlled gadget cycles explicitly reset the baseline.
- While Sentry USB owns the light, it replaces normal microSD activity signals
  and, on Pi 5, controls the red channel too. Previous triggers are restored on
  orderly stop; systemd also attempts restoration after a crash. Kernel panic,
  power loss and a hung process cannot be reliably diagnosed by this controller.
- LED failures cannot block startup, archiving or USB presentation. During an
  upgrade, the controller takes over automatically when the legacy worker exits.
- Linux fixture tests do not establish native Pi colour/polarity, visible timing,
  shutdown restoration or actual-car write behaviour. Those need hardware checks.

</details>
