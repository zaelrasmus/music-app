<script lang="ts">
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import SettingsSection from "$components/settings-section.svelte";
    import TagChip from "$components/tag-chip.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import { tagHue } from "$lib/tag-colors";
    import { promptFor } from "$lib/prompt.svelte";
    import TagIcon from "@lucide/svelte/icons/tag";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import SparklesIcon from "@lucide/svelte/icons/sparkles";
    import CheckIcon from "@lucide/svelte/icons/check";
    import MoreHorizontalIcon from "@lucide/svelte/icons/more-horizontal";

    async function rename(tagId: number, current: string) {
        const name = await promptFor("Rename tag", {
            label: "Every track carrying this tag will show the new name.",
            initial: current,
            confirmLabel: "Rename",
        });
        if (name !== null) await tagStore.rename(tagId, name);
    }
</script>

<SettingsSection
    icon={TagIcon}
    title="Tags"
    description="Colour is per tag and applies everywhere it appears. Leave one on automatic and it takes a colour of its own from the palette."
>
    {#if tagStore.tags.length === 0}
        <p class="text-muted-foreground text-[13px]">
            No tags yet. Add one from the ⋯ menu on any track.
        </p>
    {:else}
        <ul class="flex flex-col">
            {#each tagStore.tags as tag (tag.id)}
                <li
                    class="hover:bg-accent/50 flex items-center gap-3 rounded-md px-1.5 py-1.5 transition-colors"
                >
                    <TagChip id={tag.id} name={tag.name} color={tag.color} size="md" />

                    <span class="text-muted-foreground flex-1 text-xs tabular-nums">
                        {tag.trackCount}
                        {tag.trackCount === 1 ? "track" : "tracks"}
                        {#if tag.color === null}
                            <span class="opacity-70"> · automatic colour</span>
                        {/if}
                    </span>

                    <!-- Swatches, not a menu of colour names: nobody knows what
                         "fuchsia" looks like in this palette until they see it. -->
                    <div class="flex items-center gap-1">
                        {#each tagStore.palette as color (color)}
                            {@const selected = tag.color === color}
                            <button
                                type="button"
                                class="size-4 rounded-full border transition-transform hover:scale-125
                                       {selected ? 'ring-foreground/50 scale-110 ring-2' : ''}"
                                style="background-color: oklch(0.66 0.16 {tagHue(
                                    tag.id,
                                    color,
                                )}); border-color: oklch(0.5 0.16 {tagHue(tag.id, color)})"
                                aria-label="Colour {tag.name} {color}"
                                aria-pressed={selected}
                                title={color}
                                onclick={() =>
                                    tagStore.setColor(tag.id, selected ? null : color)}
                            ></button>
                        {/each}
                    </div>

                    <DropdownMenu.Root>
                        <DropdownMenu.Trigger>
                            {#snippet child({ props })}
                                <button
                                    {...props}
                                    type="button"
                                    class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-7 shrink-0 place-items-center rounded-md transition-colors"
                                    aria-label="Options for {tag.name}"
                                >
                                    <MoreHorizontalIcon class="size-4" />
                                </button>
                            {/snippet}
                        </DropdownMenu.Trigger>
                        <DropdownMenu.Content align="end" class="w-52">
                            <DropdownMenu.Item onSelect={() => rename(tag.id, tag.name)}>
                                <PencilIcon />
                                Rename
                            </DropdownMenu.Item>
                            <DropdownMenu.Item
                                disabled={tag.color === null}
                                onSelect={() => tagStore.setColor(tag.id, null)}
                            >
                                <SparklesIcon />
                                Automatic colour
                                {#if tag.color === null}
                                    <CheckIcon class="ml-auto size-4" />
                                {/if}
                            </DropdownMenu.Item>
                            <DropdownMenu.Separator />
                            <DropdownMenu.Item onSelect={() => tagStore.destroy(tag.id)}>
                                <Trash2Icon />
                                Delete tag
                            </DropdownMenu.Item>
                        </DropdownMenu.Content>
                    </DropdownMenu.Root>
                </li>
            {/each}
        </ul>

        <p class="text-muted-foreground mt-2 px-1.5 text-xs">
            Deleting a tag removes it from every track. The tracks themselves are
            untouched.
        </p>
    {/if}
</SettingsSection>
