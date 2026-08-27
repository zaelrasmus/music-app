<script lang="ts">
    import { onMount } from "svelte";
    import SettingsSection from "$components/settings-section.svelte";
    import { Button } from "$components/ui/button";
    import { details } from "$lib/details.svelte";
    import { nav } from "$lib/nav.svelte";
    import WandIcon from "@lucide/svelte/icons/wand-sparkles";

    /**
     * The way in to filling in missing artists.
     *
     * Here rather than in the sidebar because it is a job with an end: once
     * the library has artists there is nothing to come back to, and a
     * permanent sidebar entry for a finished task is clutter.
     *
     * The count is loaded on mount so the entry can say whether it is worth
     * opening. A button that will not tell you how much work is behind it is
     * one nobody presses.
     */
    onMount(() => {
        void details.load();
    });

    const missing = $derived(details.folders.reduce((n, f) => n + f.total, 0));
    const answerable = $derived(
        details.folders.reduce((n, f) => n + f.fromTitles, 0),
    );
</script>

<SettingsSection
    icon={WandIcon}
    title="Fill in missing details"
    description="Local files with no artist tag, worked out from their filenames and reviewed a folder at a time."
>
    <div class="flex items-center justify-between gap-4">
        <p class="text-muted-foreground text-[13px] leading-relaxed">
            {#if details.loading && details.folders.length === 0}
                Counting…
            {:else if missing === 0}
                Every local track has an artist.
            {:else}
                <span class="text-foreground font-medium">{missing}</span> tracks
                across
                <span class="text-foreground font-medium"
                    >{details.folders.length}</span
                >
                folders have no artist.
                <!--
                  The second number is the honest one: it says how much of this
                  the app can answer on its own, and by omission how much is a
                  judgement only the user can make.
                -->
                {answerable} of them can be read from their own titles; the rest
                depend on what the folder name means.
            {/if}
        </p>

        <Button
            variant="outline"
            class="shrink-0"
            disabled={missing === 0}
            onclick={() => nav.go("details")}
        >
            Review
        </Button>
    </div>
</SettingsSection>
