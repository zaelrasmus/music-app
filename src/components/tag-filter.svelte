<script lang="ts">
    import { Button } from "$components/ui/button";
    import { tagStore } from "$lib/tags.svelte";
    import XIcon from "@lucide/svelte/icons/x";

    interface Props {
        selectedIds: number[];
        mode: "all" | "any";
        /** Whether anything is currently narrowing the list. */
        active: boolean;
        onToggle: (tagId: number) => void;
        onModeChange: (mode: "all" | "any") => void;
        onClear: () => void;
        /** Counts come from the whole library, so they mislead inside a playlist. */
        showCounts?: boolean;
    }

    let {
        selectedIds,
        mode,
        active,
        onToggle,
        onModeChange,
        onClear,
        showCounts = true,
    }: Props = $props();
</script>

{#if tagStore.tags.length > 0}
    <div class="flex flex-wrap items-center gap-1">
        {#each tagStore.tags as tag (tag.id)}
            {@const selected = selectedIds.includes(tag.id)}
            <button
                type="button"
                class="rounded-full border px-2 py-0.5 text-xs"
                class:bg-primary={selected}
                class:text-primary-foreground={selected}
                class:border-primary={selected}
                class:text-muted-foreground={!selected}
                aria-pressed={selected}
                onclick={() => onToggle(tag.id)}
            >
                {tag.name}
                {#if showCounts}
                    <span class="opacity-60">{tag.trackCount}</span>
                {/if}
            </button>
        {/each}

        <!-- Only meaningful once two tags are selected. -->
        {#if selectedIds.length > 1}
            <button
                type="button"
                class="text-muted-foreground ml-1 text-xs underline"
                onclick={() => onModeChange(mode === "all" ? "any" : "all")}
            >
                {mode === "all" ? "matching all" : "matching any"}
            </button>
        {/if}

        {#if active}
            <Button variant="ghost" size="icon" aria-label="Clear filters" onclick={onClear}>
                <XIcon />
            </Button>
        {/if}
    </div>
{/if}
