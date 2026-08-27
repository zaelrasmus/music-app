import { readSetting, writeSetting } from "$lib/settings.svelte";

/**
 * The three optional controls, and whether they are on.
 *
 * All off by default, which is the point of the file. The player bar has room
 * for the transport, the track, and the things every listener uses; a waveform,
 * a sleep timer and an A-B loop are none of those. Shipping them switched on
 * would charge every user bar space and a background decode for features most
 * of them will never touch.
 *
 * Off is also cheaper than hidden: with the waveform off, nothing is measured
 * at all — no ffmpeg run, no stored blob — rather than measured and not drawn.
 */
class ExtrasStore {
  /** Draw the track's shape behind the seek bar. */
  waveform = $state(false);

  /** Show the sleep-timer button. */
  sleepTimer = $state(false);

  /** Show the A-B loop button. */
  abLoop = $state(false);

  async restore() {
    const [waveform, sleepTimer, abLoop] = await Promise.all([
      readSetting<boolean>("showWaveform", false),
      readSetting<boolean>("showSleepTimer", false),
      readSetting<boolean>("showAbLoop", false),
    ]);

    // Validated rather than trusted: the store is a file on disk, and a
    // non-boolean here would make every check truthy.
    this.waveform = waveform === true;
    this.sleepTimer = sleepTimer === true;
    this.abLoop = abLoop === true;
  }

  async setWaveform(on: boolean) {
    this.waveform = on;
    await writeSetting("showWaveform", on);
  }

  async setSleepTimer(on: boolean) {
    this.sleepTimer = on;
    await writeSetting("showSleepTimer", on);
  }

  async setAbLoop(on: boolean) {
    this.abLoop = on;
    await writeSetting("showAbLoop", on);
  }
}

export const extras = new ExtrasStore();
