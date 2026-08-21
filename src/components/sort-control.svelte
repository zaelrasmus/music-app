<script lang="ts">
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import {
        SORT_OPTIONS,
        type SortOption,
        sortLabel,
        type Direction,
        type Sort,
    } from "$lib/sorting";
    import ArrowUpNarrowWideIcon from "@lucide/svelte/icons/arrow-up-narrow-wide";
    import ArrowDownWideNarrowIcon from "@lucide/svelte/icons/arrow-down-wide-narrow";
    import CheckIcon from "@lucide/svelte/icons/check";
    import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";

    interface Props {
        sort: Sort;
        direction: Direction;
        /** Changes what "Default" means, so the label has to know. */
        searching?: boolean;
        /**
         * Which vocabulary to offer.
         *
         * A playlist has one option the library does not -- the order the user
         * put them in -- and no use for "Default", which means relevance while
         * searching. Passing the list rather than a flag keeps the component
         * ignorant of where it is being used.
         */
        options?: SortOption[];
        onChange: (sort: Sort, direction: Direction) => void;
        onToggleDirection: () => void;
    }

    let {
        sort,
        direction,
        searching = false,
        options = SORT_OPTIONS,
        onChange,
        onToggleDirection,
    }: Props = $props();

    const label = $derived(sortLabel(sort, searching));

    /**
     * Direction is meaningless for the default order.
     *
     * "Best match, descending" is not a thing — relevance already has one
     * correct direction — so the flip is hidden rather than shown doing
     * nothing.
     */
    // "Default" means relevance, and "Custom order" means the order the user
    // arranged -- neither has a forwards and backwards, so offering a flip on
    // them shows a control that does nothing when pressed.
    const directional = $derived(sort !== "auto" && sort !== "custom");

    const current = $derived(options.find((o) => o.id === sort));
</script>

<div class="flex items-center gap-1">
    <DropdownMenu.Root>
        <DropdownMenu.Trigger>
            {#snippet child({ props })}
                <button
                    {...props}
                    type="button"
                    class="border-border hover:bg-accent data-open:bg-accent inline-flex h-9 items-center gap-1.5 rounded-full border pr-2.5 pl-3.5 text-[13px] font-medium transition-colors"
                    aria-label="Sort by {label}"
                    title="Sort the list"
                >
                    <span class="text-muted-foreground">Sort</span>
                    {label}
                    <ChevronDownIcon class="text-muted-foreground size-3.5" />
                </button>
            {/snippet}
        </DropdownMenu.Trigger>

        <DropdownMenu.Content align="end" class="w-60">
            {#each options as option (option.id)}
                {@const selected = sort === option.id}
                <DropdownMenu.Item
                    title={option.hint}
                    onSelect={() =>
                        onChange(
                            option.id,
                            // Dates default to newest first, because that is
                            // what anyone means by "sort by date added".
                            option.id === "dateAdded" || option.id === "dateUploaded"
                                ? "desc"
                                : "asc",
                        )}
                >
                    <span class="flex-1">
                        {option.id === "auto" ? sortLabel("auto", searching) : option.label}
                    </span>
                    {#if option.hint}
                        <span class="text-muted-foreground text-[10px]">?</span>
                    {/if}
                    {#if selected}
                        <CheckIcon class="size-4" />
                    {/if}
                </DropdownMenu.Item>
            {/each}
        </DropdownMenu.Content>
    </DropdownMenu.Root>

    {#if directional}
        <!-- The label says what the arrow means for *this* field: "A – Z" and
             "Newest first" are the same direction and different words. -->
        <button
            type="button"
            class="border-border hover:bg-accent text-muted-foreground hover:text-foreground inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full border px-3 text-[13px] transition-colors"
            aria-label="Reverse the order"
            title="Reverse the order"
            onclick={onToggleDirection}
        >
            {#if direction === "asc"}
                <ArrowUpNarrowWideIcon class="size-4" />
                {current?.asc ?? "Ascending"}
            {:else}
                <ArrowDownWideNarrowIcon class="size-4" />
                {current?.desc ?? "Descending"}
            {/if}
        </button>
    {/if}
</div>
