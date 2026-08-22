<script lang="ts">
    import SettingsSection from "$components/settings-section.svelte";
    import { player } from "$lib/player.svelte";
    import AudioLinesIcon from "@lucide/svelte/icons/audio-lines";

    /**
     * Steps rather than a slider.
     *
     * A continuous control here invites fiddling with a number nobody can hear
     * themselves setting, and the useful range is small. Four choices, each a
     * step you can actually notice.
     */
    const CHOICES = [
        { db: 0, label: "Full", detail: "As mastered" },
        { db: -3, label: "−3 dB", detail: "A little" },
        { db: -6, label: "−6 dB", detail: "Half as loud" },
        { db: -12, label: "−12 dB", detail: "A quarter" },
    ];

    const current = $derived(player.volumeCeilingDb);
    const on = $derived(player.normalize);
    const gain = $derived(player.trackGainDb);
    const waiting = $derived(player.waitToMeasure);

    /** Signed, because the direction is the interesting half. */
    function formatGain(db: number) {
        const rounded = Math.round(db * 10) / 10;
        return `${rounded > 0 ? "+" : ""}${rounded.toFixed(1)} dB`;
    }
</script>

<SettingsSection
    icon={AudioLinesIcon}
    title="Even out track volume"
    description="Measures each track and corrects it towards a common loudness."
>
    <div class="flex flex-col gap-3">
        <div class="flex items-center justify-between gap-4">
            <p class="text-muted-foreground text-[13px] leading-relaxed">
                Tracks here come from files, YouTube and SoundCloud, mastered by
                different people to different levels — across this library they span
                about <span class="text-foreground font-medium">10 dB</span>, so one
                track can arrive roughly twice as loud as the one before it.
            </p>
            <!--
              Hand-built rather than a component: this is the only switch in the
              app, and the one thing it must do is be obvious at a glance which
              way it is set while an A/B is going on.
            -->
            <button
                type="button"
                role="switch"
                aria-checked={on}
                aria-label="Even out track volume"
                class="relative inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full transition-colors {on
                    ? 'bg-primary'
                    : 'bg-muted-foreground/30'}"
                onclick={() => player.setNormalize(!on)}
            >
                <span
                    class="bg-background inline-block size-5 rounded-full shadow-sm transition-transform {on
                        ? 'translate-x-[22px]'
                        : 'translate-x-0.5'}"
                ></span>
            </button>
        </div>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Each track is measured once, in the background, and played back at a gain
            that brings it towards
            <span class="text-foreground font-medium">−14 LUFS</span> — the level
            YouTube and Spotify use. The gain is decided when the track starts and
            never moves while it plays, so this is a volume correction and not a
            compressor. Nothing is filtered.
        </p>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            The next track in the queue is measured while the current one is still
            playing, so ordinary listening is levelled all the way through. Files on
            disk are measured in the background. Only a track you pick and play
            immediately, that nobody has heard before, arrives unmeasured — it plays
            as mastered and is corrected from the next time.
        </p>

        {#if on}
            <!--
              Nested under the switch above because it does nothing on its own:
              measuring a track changes nothing audible unless the correction is
              being applied.
            -->
            <div
                class="border-border/60 flex items-start justify-between gap-4 rounded-lg border p-3"
            >
                <div class="flex flex-col gap-1">
                    <span class="text-[13px] font-medium">Wait for unheard tracks</span>
                    <span class="text-muted-foreground text-xs leading-relaxed">
                        Measure a brand-new stream before playing it instead of after.
                        Levels it from the very first listen, at the cost of roughly
                        ten seconds before the sound starts. Nothing already measured,
                        queued, or on disk is affected.
                    </span>
                </div>
                <button
                    type="button"
                    role="switch"
                    aria-checked={waiting}
                    aria-label="Wait for unheard tracks"
                    class="relative mt-0.5 inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full transition-colors {waiting
                        ? 'bg-primary'
                        : 'bg-muted-foreground/30'}"
                    onclick={() => player.setWaitToMeasure(!waiting)}
                >
                    <span
                        class="bg-background inline-block size-4 rounded-full shadow-sm transition-transform {waiting
                            ? 'translate-x-[18px]'
                            : 'translate-x-0.5'}"
                    ></span>
                </button>
            </div>

            <!-- The readout is the point of the toggle: switching it while a
                 track plays is how you check whether it is doing anything, and
                 a number makes "did that change?" answerable. -->
            <p class="text-xs">
                {#if gain === null}
                    <span class="text-muted-foreground">
                        This track has not been measured yet — playing as mastered.
                    </span>
                {:else if Math.abs(gain) < 0.05}
                    <span class="text-muted-foreground">
                        This track is already at the target — no correction.
                    </span>
                {:else}
                    <span class="text-muted-foreground">This track:</span>
                    <span class="text-foreground font-medium tabular-nums"
                        >{formatGain(gain)}</span
                    >
                {/if}
            </p>
        {/if}
    </div>
</SettingsSection>

<SettingsSection
    icon={AudioLinesIcon}
    title="Maximum volume"
    description="How loud this app is allowed to get with the slider all the way up."
>
    <div class="flex flex-col gap-3">
        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Independent of the setting above: this is the ceiling the slider reaches,
            whether or not tracks are being evened out. Nothing is compressed or
            filtered — the whole slider simply stops lower.
        </p>

        <div class="flex flex-wrap items-center gap-2 pt-1">
            <div class="bg-muted flex flex-wrap items-center gap-0.5 rounded-lg p-0.5">
                {#each CHOICES as choice (choice.db)}
                    {@const selected = current === choice.db}
                    <button
                        type="button"
                        class="flex flex-col items-center rounded-md px-3 py-1.5 text-xs transition-colors {selected
                            ? 'bg-background shadow-sm'
                            : 'text-muted-foreground hover:text-foreground'}"
                        aria-pressed={selected}
                        onclick={() => player.setVolumeCeiling(choice.db)}
                    >
                        <span class="font-medium tabular-nums">{choice.label}</span>
                        <span class="text-muted-foreground text-[11px]">{choice.detail}</span>
                    </button>
                {/each}
            </div>
        </div>

        {#if current < 0}
            <!-- Said plainly, because this is the cost and it is easy to forget
                 you chose it a month later. -->
            <p class="text-muted-foreground text-xs">
                Quietly mastered tracks will be quieter here than in other players.
                That is the trade — set it back to Full if it goes too far.
            </p>
        {/if}
    </div>
</SettingsSection>
