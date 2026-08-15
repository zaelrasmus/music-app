<script lang="ts">
    import type { Component, Snippet } from "svelte";

    interface Props {
        icon?: Component;
        title: string;
        description?: string;
        actions?: Snippet;
        children: Snippet;
    }

    let { icon: Icon, title, description, actions, children }: Props = $props();
</script>

<section class="bg-card rounded-xl border">
    <header class="flex items-start justify-between gap-4 px-4 pt-4 pb-3">
        <div class="flex min-w-0 items-start gap-3">
            {#if Icon}
                <div
                    class="bg-muted text-muted-foreground mt-0.5 grid size-8 shrink-0 place-items-center rounded-lg"
                >
                    <Icon class="size-4" />
                </div>
            {/if}
            <div class="flex min-w-0 flex-col gap-0.5">
                <h2 class="text-sm font-semibold">{title}</h2>
                {#if description}
                    <p class="text-muted-foreground text-[13px] leading-relaxed">
                        {description}
                    </p>
                {/if}
            </div>
        </div>

        {#if actions}
            <div class="flex shrink-0 items-center gap-2">{@render actions()}</div>
        {/if}
    </header>

    <div class="px-4 pb-4">
        {@render children()}
    </div>
</section>
