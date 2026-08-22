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
</script>

<SettingsSection
    icon={AudioLinesIcon}
    title="Maximum volume"
    description="How loud this app is allowed to get with the slider all the way up."
>
    <div class="flex flex-col gap-3">
        <!--
          The explanation is the feature.

          This app cannot level tracks the way a streaming service does, and
          pretending otherwise with a vague setting would be worse than saying
          nothing. So the panel says what is actually happening, with the
          numbers, and offers the one control that honestly follows from it.
        -->
        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Your tracks come from files, YouTube and SoundCloud, mastered by different
            people to different levels — across this library they span about
            <span class="text-foreground font-medium">10 dB</span>, which means one
            track can arrive roughly twice as loud as the one before it. Spotify and
            YouTube measure every track and even them out before you hear a note. This
            app plays what it is given.
        </p>
        <p class="text-muted-foreground text-[13px] leading-relaxed">
            It cannot do the same, and the reason is worth knowing: measuring a track
            means decoding all of it, so a song streamed for the first time would have
            to be heard before it could be measured — which is exactly the moment a
            loud one catches you out. YouTube publishes its own measurement, but
            <code class="text-[12px]">yt-dlp</code> does not pass it on, and patching
            that would be undone by its next update.
        </p>
        <p class="text-muted-foreground text-[13px] leading-relaxed">
            So instead of guessing, this lets you set the worst case yourself. Nothing
            is compressed or filtered — the whole slider simply stops lower.
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
