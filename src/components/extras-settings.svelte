<script lang="ts">
    import SettingsSection from "$components/settings-section.svelte";
    import { extras } from "$lib/extras.svelte";
    import { waveform } from "$lib/waveform.svelte";
    import { player } from "$lib/player.svelte";
    import { abLoop } from "$lib/ab-loop.svelte";
    import { sleepStore } from "$lib/sleep.svelte";
    import SlidersIcon from "@lucide/svelte/icons/sliders-horizontal";

    async function toggleWaveform(on: boolean) {
        await extras.setWaveform(on);
        // Measured only once it is wanted, and the track already playing does
        // not wait for the next one to get its shape.
        await waveform.reload(on ? player.trackId : null);
    }

    async function toggleAbLoop(on: boolean) {
        await extras.setAbLoop(on);
        // A loop with no button to clear it would be inescapable.
        if (!on) await abLoop.clear();
    }

    async function toggleSleepTimer(on: boolean) {
        await extras.setSleepTimer(on);
        // Same reasoning: a timer nobody can see is a player that stops for no
        // visible reason.
        if (!on && sleepStore.armed) await sleepStore.cancel();
    }
</script>

<SettingsSection
    icon={SlidersIcon}
    title="Extra player controls"
    description="Off by default. The player bar has room for the transport and the track; everything here is for a particular way of listening."
>
    <div class="flex flex-col gap-4">
        {#snippet row(
            title: string,
            body: string,
            on: boolean,
            set: (on: boolean) => void,
        )}
            <div class="flex items-start justify-between gap-4">
                <div class="min-w-0">
                    <p class="text-[13px] font-medium">{title}</p>
                    <p class="text-muted-foreground text-[13px] leading-relaxed">
                        {body}
                    </p>
                </div>
                <button
                    type="button"
                    role="switch"
                    aria-checked={on}
                    aria-label={title}
                    class="relative mt-0.5 inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full transition-colors {on
                        ? 'bg-primary'
                        : 'bg-muted-foreground/30'}"
                    onclick={() => set(!on)}
                >
                    <span
                        class="bg-background inline-block size-5 rounded-full shadow-sm transition-transform {on
                            ? 'translate-x-[22px]'
                            : 'translate-x-0.5'}"
                    ></span>
                </button>
            </div>
        {/snippet}

        {@render row(
            "Waveform on the seek bar",
            "Draws the shape of the track instead of a plain bar, so a quiet passage or a drop is visible before you reach it.",
            extras.waveform,
            (on) => void toggleWaveform(on),
        )}

        {@render row(
            "Sleep timer",
            "Pauses playback after a set time, or at the end of the current track.",
            extras.sleepTimer,
            (on) => void toggleSleepTimer(on),
        )}

        {@render row(
            "A-B loop",
            "Repeats a section of a track. Press once at the start, again at the end.",
            extras.abLoop,
            (on) => void toggleAbLoop(on),
        )}

        <!--
          The cost, stated, because it is the only one of the three that has
          one and the question is reasonable to ask.
        -->
        <p class="text-muted-foreground text-[13px] leading-relaxed">
            The waveform is the only one that costs anything. Each local track is
            decoded once to measure it — under a second — and the result is kept
            as
            <span class="text-foreground font-medium">400 bytes</span> in the
            library, about a third of a megabyte for a thousand tracks. Nothing is
            measured while this is off, and streamed tracks are never measured at
            all, since drawing one would mean downloading the whole file first.
        </p>
    </div>
</SettingsSection>
