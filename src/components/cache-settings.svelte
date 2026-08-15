<script lang="ts">
    import { Button } from "$components/ui/button";
    import SettingsSection from "$components/settings-section.svelte";
    import { cacheStore, formatBytes, LIMIT_CHOICES } from "$lib/cache.svelte";
    import HardDriveIcon from "@lucide/svelte/icons/hard-drive";

    const used = $derived(formatBytes(cacheStore.usedBytes));
    const limit = $derived(formatBytes(cacheStore.limitBytes));

    /** Clamped, because the cache can briefly sit at its cap. */
    const fraction = $derived(
        cacheStore.limitBytes === 0
            ? 0
            : Math.min(cacheStore.usedBytes / cacheStore.limitBytes, 1),
    );

    const tracksCached = $derived(cacheStore.cachedIds.size);
</script>

<SettingsSection
    icon={HardDriveIcon}
    title="Streaming cache"
    description="Copies of streamed tracks, kept so replaying and seeking back do not download them again. They survive closing the app and restarting the machine; deleting them is always safe."
>
    {#snippet actions()}
        <Button
            variant="outline"
            size="sm"
            disabled={cacheStore.busy || cacheStore.usedBytes === 0}
            onclick={() => cacheStore.clear()}
        >
            Clear
        </Button>
    {/snippet}

    <div class="flex flex-col gap-3">
        <div class="flex flex-col gap-1.5">
            <div class="bg-muted h-1.5 overflow-hidden rounded-full">
                <div
                    class="bg-primary h-full rounded-full transition-[width] duration-500"
                    style="width: {fraction * 100}%"
                ></div>
            </div>
            <div class="text-muted-foreground flex items-baseline justify-between text-xs">
                <span class="tabular-nums">{used} of {limit}</span>
                {#if tracksCached > 0}
                    <span class="tabular-nums">
                        {tracksCached}
                        {tracksCached === 1 ? "track" : "tracks"} playable offline
                    </span>
                {/if}
            </div>
        </div>

        <div class="flex flex-wrap items-center gap-2">
            <span class="text-muted-foreground text-xs">Limit</span>
            <div class="bg-muted flex items-center gap-0.5 rounded-lg p-0.5">
                {#each LIMIT_CHOICES as choice (choice.bytes)}
                    {@const selected = cacheStore.limitBytes === choice.bytes}
                    <button
                        type="button"
                        class="rounded-md px-2.5 py-1 text-xs font-medium transition-colors
                               {selected
                            ? 'bg-background text-foreground shadow-sm'
                            : 'text-muted-foreground hover:text-foreground'}"
                        aria-pressed={selected}
                        disabled={cacheStore.busy}
                        onclick={() => cacheStore.setLimit(choice.bytes)}
                    >
                        {choice.label}
                    </button>
                {/each}
            </div>
        </div>

        <label
            class="hover:bg-accent/50 flex cursor-pointer items-start gap-2.5 rounded-lg p-2 transition-colors"
        >
            <input
                type="checkbox"
                class="accent-primary mt-0.5 size-3.5"
                checked={cacheStore.keepAbandoned}
                onchange={(e) => cacheStore.setKeepAbandoned(e.currentTarget.checked)}
            />
            <span class="flex flex-col gap-0.5 text-[13px]">
                <span>Keep songs you leave part-way through</span>
                <span class="text-muted-foreground text-xs">
                    Uses extra data — the rest of the track is fetched in the
                    background. Songs you play to the end are kept either way,
                    at no cost.
                </span>
            </span>
        </label>
    </div>
</SettingsSection>
