<script lang="ts">
    import SettingsSection from "$components/settings-section.svelte";
    import { player } from "$lib/player.svelte";
    import ScissorsIcon from "@lucide/svelte/icons/scissors";

    const on = $derived(player.trimSilence);
</script>

<SettingsSection
    icon={ScissorsIcon}
    title="Skip silence at the end of a track"
    description="Ends a track when its music does, rather than when its file does."
>
    <div class="flex flex-col gap-3">
        <div class="flex items-center justify-between gap-4">
            <p class="text-muted-foreground text-[13px] leading-relaxed">
                Uploads often run on after the last note. Two tracks measured from
                this library carry
                <span class="text-foreground font-medium">14.6</span> and
                <span class="text-foreground font-medium">5.6</span> seconds of
                encoded silence at the end — a gap that is in the recording, not in
                the player.
            </p>
            <button
                type="button"
                role="switch"
                aria-checked={on}
                aria-label="Skip silence at the end of a track"
                class="relative inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full transition-colors {on
                    ? 'bg-primary'
                    : 'bg-muted-foreground/30'}"
                onclick={() => player.setTrimSilence(!on)}
            >
                <span
                    class="bg-background inline-block size-5 rounded-full shadow-sm transition-transform {on
                        ? 'translate-x-[22px]'
                        : 'translate-x-0.5'}"
                ></span>
            </button>
        </div>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Separate from gapless on purpose, because they fix different things.
            Gapless stops <em>this player</em> putting a gap between two tracks; this
            skips the gap that is already inside the recording. Either is useful without
            the other.
        </p>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Deliberately cautious. It needs
            <span class="text-foreground font-medium">three seconds</span> of true
            digital silence, beginning within
            <span class="text-foreground font-medium">twenty seconds</span> of the end,
            on a track that has already been heard — so a quiet passage, a dramatic
            pause, a track that is silent throughout, and a stream that has stalled
            all play through untouched. You will see it as the progress bar moving
            on a few seconds early.
        </p>
    </div>
</SettingsSection>
