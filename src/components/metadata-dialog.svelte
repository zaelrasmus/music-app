<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import { metadataEditor } from "$lib/metadata.svelte";
    import RotateIcon from "@lucide/svelte/icons/rotate-ccw";

    /**
     * The metadata editor's modal.
     *
     * Native `<dialog>` for the same reasons `prompt-dialog` uses one: a focus
     * trap, Escape-to-close, inertness behind it and top-layer stacking, none
     * of which are worth reimplementing.
     */
    let dialog = $state<HTMLDialogElement | null>(null);
    let titleInput = $state<HTMLInputElement | null>(null);

    $effect(() => {
        if (!dialog) return;

        if (metadataEditor.open && !dialog.open) {
            dialog.showModal();
            // Selected rather than focused: the usual edit replaces the title
            // outright, and the file name it was scanned from is rarely a
            // useful starting point to type into.
            titleInput?.select();
        } else if (!metadataEditor.open && dialog.open) {
            dialog.close();
        }
    });

    /** What the file holds for a field, drawn as absent rather than empty. */
    function tagText(value: string | null) {
        return value ?? "—";
    }
</script>

<dialog
    bind:this={dialog}
    class="bg-popover text-popover-foreground m-auto w-[min(30rem,calc(100vw-2rem))] rounded-xl border p-0 shadow-2xl backdrop:bg-black/45 backdrop:backdrop-blur-[2px]"
    onclose={() => metadataEditor.close()}
    oncancel={(e) => {
        e.preventDefault();
        metadataEditor.close();
    }}
>
    {#if metadataEditor.track}
        {@const track = metadataEditor.track}
        <form
            class="flex flex-col gap-4 p-5"
            onsubmit={(e) => {
                e.preventDefault();
                void metadataEditor.save();
            }}
        >
            <div class="flex flex-col gap-1">
                <h2 class="text-base font-semibold">Edit details</h2>
                <p class="text-muted-foreground text-sm">
                    {#if track.source === "local"}
                        How this track is shown in your library.
                    {:else}
                        How this track is shown. What {track.source === "youtube"
                            ? "YouTube"
                            : "SoundCloud"} calls it is kept separately and is not
                        changed.
                    {/if}
                </p>
            </div>

            <div class="flex flex-col gap-3">
                <label class="flex flex-col gap-1.5">
                    <span class="text-[13px] font-medium">Title</span>
                    <Input bind:ref={titleInput} bind:value={metadataEditor.title} />
                </label>

                <label class="flex flex-col gap-1.5">
                    <span class="text-[13px] font-medium">Artist</span>
                    <Input
                        bind:value={metadataEditor.artist}
                        placeholder="Unknown"
                    />
                </label>

                <label class="flex flex-col gap-1.5">
                    <span class="text-[13px] font-medium">Album</span>
                    <Input
                        bind:value={metadataEditor.album}
                        placeholder="Unknown"
                    />
                </label>
            </div>

            <!--
              Shown only when the file disagrees, because that is the only time
              it says anything. A panel repeating the three boxes above would
              be noise on every track nobody has edited.
            -->
            {#if metadataEditor.fileTags && metadataEditor.differsFromFile}
                {@const file = metadataEditor.fileTags}
                <div class="bg-muted/40 flex flex-col gap-2 rounded-lg border p-3">
                    <div class="flex items-start justify-between gap-3">
                        <div class="min-w-0">
                            <p class="text-[13px] font-medium">
                                The file's own tags say
                            </p>
                            <!--
                              The point of stating this: a rescan will not
                              overwrite what is typed above, so the two can
                              disagree indefinitely and nothing else would ever
                              mention it.
                            -->
                            <p
                                class="text-muted-foreground mt-0.5 text-[13px] leading-relaxed"
                            >
                                Your version is kept. Rescans will not replace
                                it.
                            </p>
                        </div>
                        <Button
                            type="button"
                            variant="ghost"
                            size="sm"
                            class="shrink-0"
                            onclick={() => metadataEditor.useFileTags()}
                        >
                            <RotateIcon class="size-3.5" />
                            Use these
                        </Button>
                    </div>

                    <dl
                        class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[13px]"
                    >
                        <dt class="text-muted-foreground">Title</dt>
                        <dd class="truncate">{file.title}</dd>
                        <dt class="text-muted-foreground">Artist</dt>
                        <dd class="truncate">{tagText(file.artist)}</dd>
                        <dt class="text-muted-foreground">Album</dt>
                        <dd class="truncate">{tagText(file.album)}</dd>
                    </dl>
                </div>
            {/if}

            <div class="flex justify-end gap-2">
                <Button
                    type="button"
                    variant="ghost"
                    onclick={() => metadataEditor.close()}
                >
                    Cancel
                </Button>
                <Button
                    type="submit"
                    disabled={metadataEditor.title.trim() === "" ||
                        metadataEditor.saving}
                >
                    Save
                </Button>
            </div>
        </form>
    {/if}
</dialog>
