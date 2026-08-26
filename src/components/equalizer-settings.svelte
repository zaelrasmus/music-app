<script lang="ts">
    import SettingsSection from "$components/settings-section.svelte";
    import {
        equalizer,
        PRESETS,
        MAX_GAIN_DB,
        bandLabel,
        type Preset,
    } from "$lib/equalizer.svelte";
    import SlidersIcon from "@lucide/svelte/icons/sliders-horizontal";
    import RotateCcwIcon from "@lucide/svelte/icons/rotate-ccw";

    const on = $derived(equalizer.enabled);
    const gains = $derived(equalizer.gains);
    const centres = $derived(equalizer.centres);
    const active = $derived(equalizer.activePreset);

    /** Signed, because which way a band went is the readable part. */
    function formatGain(db: number) {
        if (db === 0) return "0";
        return `${db > 0 ? "+" : "−"}${Math.abs(db)}`;
    }

    function apply(preset: Preset) {
        void equalizer.applyPreset(preset);
    }
</script>

<SettingsSection
    icon={SlidersIcon}
    title="Equalizer"
    description="Ten bands, applied to everything — files and streams alike."
>
    {#snippet actions()}
        <button
            type="button"
            class="text-muted-foreground hover:bg-accent hover:text-foreground flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors disabled:opacity-40 disabled:hover:bg-transparent"
            disabled={equalizer.isFlat}
            onclick={() => equalizer.reset()}
        >
            <RotateCcwIcon class="size-3.5" />
            Reset
        </button>

        <!-- Same hand-built switch as the loudness panel, for the same
             reason: which way it is set has to be obvious mid-comparison. -->
        <button
            type="button"
            role="switch"
            aria-checked={on}
            aria-label="Equalizer"
            class="relative inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full transition-colors {on
                ? 'bg-primary'
                : 'bg-muted-foreground/30'}"
            onclick={() => equalizer.setEnabled(!on)}
        >
            <span
                class="bg-background inline-block size-5 rounded-full shadow-sm transition-transform {on
                    ? 'translate-x-[22px]'
                    : 'translate-x-0.5'}"
            ></span>
        </button>
    {/snippet}

    <div class="flex flex-col gap-4">
        <!--
          Presets first.

          Ten sliders is a lot to face with no idea where to start, and most
          people want a shape rather than a curve. The presets are the way in;
          the sliders are there for when one is nearly right.
        -->
        <div class="flex flex-wrap gap-1.5">
            {#each PRESETS as preset (preset.name)}
                <button
                    type="button"
                    class="rounded-full border px-3 py-1 text-xs transition-colors {active ===
                    preset.name
                        ? 'border-transparent bg-foreground text-background font-medium'
                        : 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'}"
                    aria-pressed={active === preset.name}
                    onclick={() => apply(preset)}
                >
                    {preset.name}
                </button>
            {/each}
        </div>

        <!--
          Vertical sliders, laid out like the frequency axis they represent:
          low on the left, high on the right. Horizontal rows would be a list
          of ten numbers, which is not a curve you can read at a glance.
        -->
        <div
            class="bg-muted/30 flex items-end justify-between gap-1 rounded-lg px-2 py-3 transition-opacity {on
                ? ''
                : 'opacity-50'}"
        >
            {#each gains as gain, index (index)}
                <div class="flex min-w-0 flex-1 flex-col items-center gap-1.5">
                    <span
                        class="text-[10px] tabular-nums {gain === 0
                            ? 'text-muted-foreground'
                            : 'text-foreground font-medium'}"
                    >
                        {formatGain(gain)}
                    </span>

                    <!--
                      `writing-mode: vertical-lr` with a flip, rather than a
                      rotate: a rotated input keeps its original hit box, so the
                      thumb ends up somewhere other than where it is drawn.
                    -->
                    <input
                        type="range"
                        class="eq-slider accent-primary h-24 cursor-pointer"
                        min={-MAX_GAIN_DB}
                        max={MAX_GAIN_DB}
                        step="1"
                        value={gain}
                        disabled={!on}
                        aria-label="{centres[index]
                            ? bandLabel(centres[index]) + ' hertz'
                            : `Band ${index + 1}`}"
                        oninput={(e) =>
                            equalizer.setBand(
                                index,
                                Number(e.currentTarget.value),
                            )}
                    />

                    <span class="text-muted-foreground text-[10px] tabular-nums">
                        {centres[index] ? bandLabel(centres[index]) : index + 1}
                    </span>
                </div>
            {/each}
        </div>

        <p class="text-muted-foreground text-[13px] leading-relaxed">
            Bands are one octave apart, from
            <span class="text-foreground font-medium">31 Hz</span> to
            <span class="text-foreground font-medium">16 kHz</span>. Boosting can
            push a loud track past full scale; the limiter catches that, so a heavy
            curve costs headroom rather than causing distortion. With every band at
            zero the audio is passed through untouched.
        </p>
    </div>
</SettingsSection>

<style>
    /* Vertical, and the right way up — low values at the bottom. */
    .eq-slider {
        writing-mode: vertical-lr;
        direction: rtl;
        width: 1.25rem;
    }
</style>
