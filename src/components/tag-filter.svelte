<script lang="ts">
    import TagChip from "$components/tag-chip.svelte";
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
    <div class="flex flex-wrap items-center gap-1.5">
        {#each tagStore.tags as tag (tag.id)}
            <TagChip
                id={tag.id}
                name={tag.name}
                color={tag.color}
                count={showCounts ? tag.trackCount : null}
                selected={selectedIds.includes(tag.id)}
                onclick={() => onToggle(tag.id)}
            />
        {/each}

        <!-- Only meaningful once two tags are selected. -->
        {#if selectedIds.length > 1}
            <button
                type="button"
                class="text-muted-foreground hover:text-foreground ml-1 text-[11px] underline underline-offset-2 transition-colors"
                title="Switch between tracks carrying every selected tag and tracks carrying any of them"
                onclick={() => onModeChange(mode === "all" ? "any" : "all")}
            >
                {mode === "all" ? "matching all" : "matching any"}
            </button>
        {/if}

        {#if active}
            <button
                type="button"
                class="text-muted-foreground hover:bg-accent hover:text-foreground inline-grid size-6 place-items-center rounded-full transition-colors"
                aria-label="Clear filters"
                title="Clear filters"
                onclick={onClear}
            >
                <XIcon class="size-3.5" />
            </button>
        {/if}
    </div>
{/if}
