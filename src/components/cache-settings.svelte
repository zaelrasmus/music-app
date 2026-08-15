<script lang="ts">
    import { Button } from "$components/ui/button";
    import {
        cacheStore,
        formatBytes,
        LIMIT_CHOICES,
    } from "$lib/cache.svelte";
    import HardDriveIcon from "@lucide/svelte/icons/hard-drive";

    const used = $derived(formatBytes(cacheStore.usedBytes));
    const limit = $derived(formatBytes(cacheStore.limitBytes));

    /** Clamped, because the cache can briefly sit at its cap. */
    const fraction = $derived(
        cacheStore.limitBytes === 0
            ? 0
            : Math.min(cacheStore.usedBytes / cacheStore.limitBytes, 1),
    );
</script>

<section class="flex flex-col gap-2">
    <div class="flex items-center justify-between gap-3">
        <div class="flex flex-col gap-0.5">
            <h2 class="flex items-center gap-2 text-sm font-semibold">
                <HardDriveIcon class="size-4" />
                Streaming cache
            </h2>
            <p class="text-muted-foreground text-xs">
                Copies of streamed tracks, kept so replaying and seeking back do
                not download them again. Deleting them is always safe.
            </p>
        </div>

        <Button
            variant="outline"
            size="sm"
            disabled={cacheStore.busy || cacheStore.usedBytes === 0}
            onclick={() => cacheStore.clear()}
        >
            Clear
        </Button>
    </div>

    <div class="bg-muted h-1.5 overflow-hidden rounded-full">
        <div
            class="bg-primary h-full rounded-full transition-[width]"
            style="width: {fraction * 100}%"
        ></div>
    </div>

    <div class="flex items-center justify-between gap-3">
        <span class="text-muted-foreground text-xs tabular-nums">
            {used} of {limit}
        </span>

        <div class="flex items-center gap-1">
            {#each LIMIT_CHOICES as choice (choice.bytes)}
                {@const selected = cacheStore.limitBytes === choice.bytes}
                <button
                    type="button"
                    class="rounded-full border px-2 py-0.5 text-[11px]"
                    class:bg-primary={selected}
                    class:text-primary-foreground={selected}
                    class:border-primary={selected}
                    class:text-muted-foreground={!selected}
                    aria-pressed={selected}
                    disabled={cacheStore.busy}
                    onclick={() => cacheStore.setLimit(choice.bytes)}
                >
                    {choice.label}
                </button>
            {/each}
        </div>
    </div>
</section>
