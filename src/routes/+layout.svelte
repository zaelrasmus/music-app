<script lang="ts">
	import './layout.css';
	  import { Toaster } from "$components/ui/sonner/index.js";

	const { children } = $props();
</script>

<Toaster />

{@render children()}
