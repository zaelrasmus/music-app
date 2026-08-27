<script lang="ts">
    import SettingsSection from "$components/settings-section.svelte";
    import { player } from "$lib/player.svelte";
    import ArrowRightLeftIcon from "@lucide/svelte/icons/arrow-right-left";

    const on = $derived(player.gapless);
</script>

<SettingsSection
    icon={ArrowRightLeftIcon}
    title="Play albums without gaps"
    description="Hands one track to the next without stopping the audio in between."
>
    <div class="flex flex-col gap-3">
        <div class="flex items-center justify-between gap-4">
            <p class="text-muted-foreground text-[13px] leading-relaxed">
                Some records were recorded to run together — a live set, a DJ mix,
                the movements of one piece. A short silence between two of those
                tracks is not neutral: it is a seam where the artist put none.
            </p>
            <!--
              Same switch as the loudness panel, deliberately: two settings that
              do the same kind of thing should not look like two different
              kinds of control.
            -->
            <button
                type="button"
                role="switch"
                aria-checked={on}
                aria-label="Play albums without gaps"
                class="relative inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full transition-colors {on
                    ? 'bg-primary'
                    : 'bg-muted-foreground/30'}"
                onclick={() => player.setGapless(!on)}
            >
                <span
                    class="bg-background inline-block size-5 rounded-full shadow-sm transition-transform {on
                        ? 'translate-x-[22px]'
                        : 'translate-x-0.5'}"
                ></span>
            </button>
        </div>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            The next track is decoded while the current one is still playing, and
            handed to the audio device a few seconds before it is needed — so the
            two are already joined by the time the first one ends. Each still
            carries its own volume correction, so evening out track volume keeps
            working across the join.
        </p>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            This only changes what happens when a track is
            <span class="text-foreground font-medium">allowed to end</span>.
            Skipping, seeking, or reordering the queue all take the ordinary path,
            with the same short pause they have always had — and turning this off
            makes every track take that path.
        </p>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            It cannot help with silence that is inside the recording itself, which
            many uploads carry at the end. That is what
            <span class="text-foreground font-medium"
                >Skip silence at the end of a track</span
            > is for.
        </p>
    </div>
</SettingsSection>
