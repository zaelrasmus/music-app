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


    /**
     * The targets worth offering, with what each is actually for.
     *
     * Steps rather than a slider for the same reason as the ceiling: this is a
     * number nobody can set by ear, and four labelled choices are more useful
     * than a continuous control that invites landing on -13.4.
     *
     * The range stops at -23 and -9 because the backend clamps there: quieter
     * is broadcast territory and would leave this library barely audible on a
     * laptop, louder asks for more boost than most tracks have headroom for.
     */
    const TARGETS = [
        {
            lufs: -18,
            label: "−18",
            detail: "Quiet",
            guide: "More headroom, so almost nothing is pulled down by the limiter. Good on headphones or a real hi-fi, where you can just turn it up.",
        },
        {
            lufs: -14,
            label: "−14",
            detail: "Standard",
            guide: "What YouTube and Spotify use, and the level most of this library was mastered near. Leave it here unless something bothers you.",
        },
        {
            lufs: -11,
            label: "−11",
            detail: "Loud",
            guide: "Closer to how a phone or laptop speaker wants to be driven. Quiet tracks get a real lift; loud ones have less room, so the limiter works harder.",
        },
        {
            lufs: -9,
            label: "−9",
            detail: "Loudest",
            guide: "As far as this goes. Most tracks have no headroom for it, so expect the limiter to be doing something on nearly everything. Useful in a noisy room, not for listening closely.",
        },
    ];
    const current = $derived(player.volumeCeilingDb);
    const on = $derived(player.normalize);
    const gain = $derived(player.trackGainDb);

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
            Each track is played at a gain that brings it towards
            <span class="text-foreground font-medium">−14 LUFS</span> — the level
            YouTube and Spotify use. It is a volume correction and not a compressor:
            one number per track, nothing filtered, nothing squashed.
        </p>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Files on disk are measured in the background, and so is every track you
            have already heard. A stream nobody has played has nothing to measure
            yet, so it is sampled instead — four short pieces spread across its
            length, fetched while the song is already playing. That lands within
            <span class="text-foreground font-medium">1 dB</span> of a full
            measurement on every track tested here, and the correction fades in a
            second or two in rather than switching, so you should not hear it
            arrive. The exact figure is taken afterwards from the copy the stream
            left behind, and is what every later play uses.
        </p>

        {#if on}


            <!--
              Steps, and each one labelled with what it is *for* rather than
              what it is. "-14 LUFS" means nothing to most people; "what YouTube
              and Spotify use" is the same fact in a form you can act on.
            -->
            <div class="border-border/60 flex flex-col gap-2 rounded-lg border p-3">
                <div class="flex items-baseline justify-between gap-3">
                    <span class="text-[13px] font-medium">Target loudness</span>
                    <span class="text-muted-foreground text-xs tabular-nums">
                        {player.targetLufs} LUFS
                    </span>
                </div>

                <div class="bg-muted flex flex-wrap items-center gap-0.5 rounded-lg p-0.5">
                    {#each TARGETS as choice (choice.lufs)}
                        {@const selected = player.targetLufs === choice.lufs}
                        <button
                            type="button"
                            class="flex flex-1 flex-col items-center rounded-md px-2 py-1.5 text-xs transition-colors {selected
                                ? 'bg-background shadow-sm'
                                : 'text-muted-foreground hover:text-foreground'}"
                            aria-pressed={selected}
                            onclick={() => player.setTargetLufs(choice.lufs)}
                        >
                            <span class="font-medium tabular-nums">{choice.label}</span>
                            <span class="text-muted-foreground text-[11px]">
                                {choice.detail}
                            </span>
                        </button>
                    {/each}
                </div>

                <p class="text-muted-foreground text-xs leading-relaxed">
                    {TARGETS.find((t) => t.lufs === player.targetLufs)?.guide ??
                        "A custom target."}
                </p>
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
