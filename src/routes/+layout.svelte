<script lang="ts">
	import './layout.css';
	import { Toaster } from "$components/ui/sonner/index.js";
	import PlayerBar from "$components/player-bar.svelte";
	import QueuePanel from "$components/queue-panel.svelte";

	const { children } = $props();
</script>

<Toaster />

<!-- Bottom padding keeps the last row clear of the fixed player bar. -->
<div class="pb-28">
	{@render children()}
</div>

<!-- Both are fixed to the viewport; the panel sits above the bar. -->
<QueuePanel />
<PlayerBar />
