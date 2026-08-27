import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { readSetting, writeSetting } from "$lib/settings.svelte";

export type OutputDevice = {
  id: string;
  name: string;
  /** What the system would pick on its own, right now. */
  isDefault: boolean;
};

type OutputStatus = {
  devices: OutputDevice[];
  /** The device sound is actually coming out of. */
  activeId: string | null;
  activeName: string | null;
};

/**
 * Which speakers the music comes out of.
 *
 * The choice is a device id and the alternative to it is `null` — "follow the
 * system" — which is deliberately not the same as picking whichever device is
 * the default today. One tracks Windows from then on; the other pins this
 * endpoint and stays there when a monitor is plugged in and steals the default.
 *
 * Nothing here decides what actually plays. The engine owns that rule (the
 * chosen device when it is connected, the system default when it is not), and
 * `activeId` is it *reporting back* rather than this store predicting it — a
 * second copy of that rule is exactly how a picker ends up naming a device the
 * music is not coming out of.
 */
class DeviceStore {
  devices = $state<OutputDevice[]>([]);
  /** The saved choice. `null` is "follow the system". */
  chosen = $state<string | null>(null);
  activeId = $state<string | null>(null);
  activeName = $state<string | null>(null);
  loading = $state(false);

  /** The chosen device, when it is one of the devices present. */
  chosenDevice = $derived(
    this.chosen === null
      ? null
      : (this.devices.find((device) => device.id === this.chosen) ?? null),
  );

  /**
   * Whether the choice cannot be honoured because the device is not here.
   *
   * The state worth naming on screen: sound is playing, the setting still says
   * what it said, and the two do not match. Saying nothing would make the
   * picker look broken; clearing the choice would throw away a preference over
   * an unplugged cable.
   */
  chosenIsAway = $derived(this.chosen !== null && this.chosenDevice === null);

  /**
   * Reads the saved choice and puts the engine on it.
   *
   * Both halves, because this store is the only owner of the setting. Reading
   * it here and sending it from the player's restore would be two places that
   * have to agree about a key, which is the arrangement that eventually
   * disagrees.
   *
   * Must finish before `restorePlayback`, or a track starts on one device and
   * is rebuilt onto another a moment later — a gap at the start of every
   * session. `+page.svelte` is what orders the two.
   *
   * The id is sent without checking it against the devices present: a saved
   * choice for something that is not plugged in right now is the case the
   * engine is built to hold on to.
   */
  async restore() {
    this.chosen = await readSetting<string | null>("outputDevice", null);

    // Sent even when null, so nothing carries over from a previous run of the
    // audio thread and "follow the system" is a state that is actually set.
    try {
      await invoke("set_output_device", { id: this.chosen });
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Re-reads the devices present and where sound is going.
   *
   * Called whenever the settings page opens rather than cached, because a
   * cached list is precisely the thing that would still be offering headphones
   * five seconds after they were unplugged. Measured at 8.3 ms.
   */
  async refresh() {
    this.loading = true;
    try {
      const status = await invoke<OutputStatus>("output_status");
      this.devices = status.devices;
      this.activeId = status.activeId;
      this.activeName = status.activeName;
    } catch (e) {
      toast.error(String(e));
    } finally {
      this.loading = false;
    }
  }

  /**
   * Sends playback to `id`, or back to the system's own choice with `null`.
   *
   * Saved before it is sent. The engine holds on to a choice whose device is
   * absent — that is the whole reason `chosenIsAway` exists — so refusing to
   * persist one until it succeeded would break reconnecting far more often
   * than it would prevent a bad value.
   */
  async choose(id: string | null) {
    if (id === this.chosen) return;

    this.chosen = id;
    await writeSetting("outputDevice", id);

    try {
      await invoke("set_output_device", { id });
    } catch (e) {
      toast.error(String(e));
    }
    // Deliberately not refreshed here. The engine reopens on its next tick and
    // opening a stream takes as long as it takes, so any single delay after
    // this is a guess -- and a wrong guess reads back the device being left.
    // `watch` is what keeps the panel true.
  }

  /**
   * Keeps the list current for as long as somebody is looking at it.
   *
   * Polling, because the alternatives are worse for what this costs. The
   * devices present are not something the app is told about -- cpal has no
   * notification API, which is the same reason `device_watch` polls -- and
   * where sound *ends up* is only known once a reopen has finished, which is
   * not a moment any single delay after a click can be timed against.
   *
   * 8.3 ms every two seconds, and only while the settings page is open.
   */
  watch() {
    void this.refresh();
    const timer = setInterval(() => void this.refresh(), 2000);
    return () => clearInterval(timer);
  }
}

export const devices = new DeviceStore();
