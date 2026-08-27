<script lang="ts">
    import { onMount } from "svelte";
    import SettingsSection from "$components/settings-section.svelte";
    import { devices } from "$lib/devices.svelte";
    import SpeakerIcon from "@lucide/svelte/icons/speaker";
    import CheckIcon from "@lucide/svelte/icons/check";
    import Volume2Icon from "@lucide/svelte/icons/volume-2";
    import TriangleAlertIcon from "@lucide/svelte/icons/triangle-alert";

    /**
     * Choosing which speakers the music comes out of.
     *
     * The list is live for as long as this is on screen. Devices come and go
     * with a cable, and a picker showing a pair of headphones that left the
     * building is worse than no picker — see `devices.watch`.
     */
    onMount(() => devices.watch());

    /** Whichever entry the system default resolves to, for the first row. */
    const systemDefault = $derived(
        devices.devices.find((device) => device.isDefault) ?? null,
    );
</script>

<SettingsSection
    icon={SpeakerIcon}
    title="Output device"
    description="Where the music is played. Changing this rebuilds the current track on the new device, so expect a short break and then the same song."
>
    <div class="flex flex-col gap-2">
        <!--
          Its own row above the devices, because it is a different kind of
          answer. Every other row names one endpoint and stays on it; this one
          says "whatever Windows is using", and follows it when that changes.
          Picking the device that happens to be default today would look the
          same and behave differently the next time a monitor is plugged in.
        -->
        <button
            type="button"
            class="hover:bg-accent/50 flex items-center gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors
                   {devices.chosen === null
                ? 'border-primary bg-primary/10'
                : 'border-transparent'}"
            aria-pressed={devices.chosen === null}
            onclick={() => devices.choose(null)}
        >
            <div class="min-w-0 flex-1">
                <p class="text-[13px] font-medium">Follow the system</p>
                <p class="text-muted-foreground truncate text-xs">
                    {#if systemDefault}
                        Currently {systemDefault.name}. Moves with whatever Windows
                        is set to.
                    {:else}
                        Moves with whatever Windows is set to.
                    {/if}
                </p>
            </div>
            {#if devices.chosen === null}
                <CheckIcon class="text-primary size-4 shrink-0" />
            {/if}
        </button>

        {#if devices.devices.length === 0}
            <p class="text-muted-foreground px-3 py-2 text-[13px]">
                {devices.loading ? "Looking for devices…" : "No output devices found."}
            </p>
        {/if}

        {#each devices.devices as device (device.id)}
            {@const chosen = devices.chosen === device.id}
            {@const playing = devices.activeId === device.id}
            <button
                type="button"
                class="hover:bg-accent/50 flex items-center gap-3 rounded-lg border px-3 py-2.5 text-left transition-colors
                       {chosen ? 'border-primary bg-primary/10' : 'border-transparent'}"
                aria-pressed={chosen}
                onclick={() => devices.choose(device.id)}
            >
                <div class="min-w-0 flex-1">
                    <p class="truncate text-[13px] font-medium">{device.name}</p>
                    <!--
                      Two different facts, and they can disagree: this device
                      is what the system would pick, and this device is what
                      sound is actually coming out of. Both are worth saying
                      only when true.
                    -->
                    {#if device.isDefault || playing}
                        <p
                            class="text-muted-foreground flex items-center gap-1.5 text-xs"
                        >
                            {#if playing}
                                <Volume2Icon class="size-3 shrink-0" />
                                Playing here
                            {/if}
                            {#if playing && device.isDefault}·{/if}
                            {#if device.isDefault}System default{/if}
                        </p>
                    {/if}
                </div>
                {#if chosen}
                    <CheckIcon class="text-primary size-4 shrink-0" />
                {/if}
            </button>
        {/each}

        <!--
          The state the whole fallback exists for, said plainly. Sound is
          playing, the setting still says what it said, and the two do not
          match — which without a line here looks like the picker ignored the
          click somebody made three days ago.
        -->
        {#if devices.chosenIsAway}
            <div
                class="border-muted-foreground/30 bg-muted/40 mt-1 flex items-start gap-2.5 rounded-lg border px-3 py-2.5"
            >
                <TriangleAlertIcon
                    class="text-muted-foreground mt-0.5 size-4 shrink-0"
                />
                <p class="text-muted-foreground text-[13px] leading-relaxed">
                    The device you chose is not connected. Sound is going to
                    <span class="text-foreground font-medium"
                        >{devices.activeName ?? "the system default"}</span
                    >
                    for now, and moves back on its own when the device returns —
                    the choice is kept, not forgotten.
                </p>
            </div>
        {/if}
    </div>
</SettingsSection>
