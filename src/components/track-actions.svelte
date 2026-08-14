<script lang="ts">
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import { Button } from "$components/ui/button";
    import { player } from "$lib/player.svelte";
    import ListPlusIcon from "@lucide/svelte/icons/list-plus";
    import CornerUpRightIcon from "@lucide/svelte/icons/corner-up-right";

    interface Props {
        /**
         * Produces the track id to queue.
         *
         * A function rather than a value because a YouTube search result is
         * not a track yet — it has to be saved first, and that should only
         * happen if the user actually queues it.
         */
        resolveTrackId: () => Promise<number | null>;
        label?: string;
    }

    let { resolveTrackId, label = "Queue" }: Props = $props();

    let busy = $state(false);

    async function queue(where: "next" | "last") {
        busy = true;
        try {
            const trackId = await resolveTrackId();
            if (trackId === null) return;

            if (where === "next") {
                await player.playNext(trackId);
            } else {
                await player.addToQueue(trackId);
            }
        } finally {
            busy = false;
        }
    }
</script>

<DropdownMenu.Root>
    <DropdownMenu.Trigger>
        {#snippet child({ props })}
            <Button
                {...props}
                variant="ghost"
                size="icon"
                aria-label={label}
                title={label}
                disabled={busy}
            >
                <ListPlusIcon />
            </Button>
        {/snippet}
    </DropdownMenu.Trigger>

    <DropdownMenu.Content align="end" class="w-48">
        <DropdownMenu.Item onSelect={() => queue("next")}>
            <CornerUpRightIcon />
            Play next
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={() => queue("last")}>
            <ListPlusIcon />
            Add to queue
        </DropdownMenu.Item>
    </DropdownMenu.Content>
</DropdownMenu.Root>
