<script lang="ts">
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import { mode, userPrefersMode, setMode, resetMode } from "mode-watcher";
    import SunIcon from "@lucide/svelte/icons/sun";
    import MoonIcon from "@lucide/svelte/icons/moon";
    import MonitorIcon from "@lucide/svelte/icons/monitor";
    import CheckIcon from "@lucide/svelte/icons/check";

    interface Props {
        /** Titlebar buttons are flatter and dimmer than buttons in the page. */
        chrome?: boolean;
    }

    let { chrome = false }: Props = $props();

    /**
     * Three states, not two.
     *
     * "System" is a real choice and not the same as whichever of light or dark
     * it currently resolves to -- a two-way toggle silently pins the theme the
     * first time it is touched, and the app then stops following the OS at
     * dusk without ever having said so.
     */
    const preference = $derived(userPrefersMode.current);
    const resolved = $derived(mode.current ?? "light");

    const label = $derived(
        preference === "system"
            ? `Theme: follows system (${resolved})`
            : `Theme: ${preference}`,
    );

    const trigger = $derived(
        chrome
            ? "inline-grid h-8 w-9 place-items-center rounded-md text-titlebar-foreground transition-colors hover:bg-foreground/10 hover:text-foreground focus-visible:outline-none focus-visible:bg-foreground/10"
            : "inline-grid size-9 place-items-center rounded-md border transition-colors hover:bg-accent",
    );
</script>

<DropdownMenu.Root>
    <DropdownMenu.Trigger>
        {#snippet child({ props })}
            <button {...props} type="button" class={trigger} aria-label={label} title={label}>
                {#if preference === "system"}
                    <MonitorIcon class="size-4" />
                {:else if resolved === "dark"}
                    <MoonIcon class="size-4" />
                {:else}
                    <SunIcon class="size-4" />
                {/if}
            </button>
        {/snippet}
    </DropdownMenu.Trigger>

    <DropdownMenu.Content align="end" class="w-44">
        <DropdownMenu.Item onSelect={() => setMode("light")}>
            <SunIcon />
            Light
            {#if preference === "light"}
                <CheckIcon class="ml-auto size-4" />
            {/if}
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={() => setMode("dark")}>
            <MoonIcon />
            Dark
            {#if preference === "dark"}
                <CheckIcon class="ml-auto size-4" />
            {/if}
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={() => resetMode()}>
            <MonitorIcon />
            System
            {#if preference === "system"}
                <CheckIcon class="ml-auto size-4" />
            {/if}
        </DropdownMenu.Item>
    </DropdownMenu.Content>
</DropdownMenu.Root>
