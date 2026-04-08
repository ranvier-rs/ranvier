<script lang="ts">
	import { goto } from '$app/navigation';

	let token = '';

	if (typeof window !== 'undefined') {
		token = sessionStorage.getItem('reference-fullstack-admin-token') ?? '';
	}

	async function logout() {
		if (typeof window !== 'undefined') {
			sessionStorage.removeItem('reference-fullstack-admin-token');
		}
		token = '';
		await goto('/login');
	}
</script>

<div class="shell">
	<header>
		<div>
			<p class="eyebrow">Reference App</p>
			<h1>Reference Fullstack Admin</h1>
		</div>
		<nav>
			<a href="/">Dashboard</a>
			<a href="/users">Users</a>
			<a href="/login">Login</a>
			{#if token}
				<button on:click={logout}>Logout</button>
			{/if}
		</nav>
	</header>
	<main>
		<slot />
	</main>
</div>

<style>
	:global(body) {
		margin: 0;
		font-family: "Segoe UI", sans-serif;
		background: linear-gradient(180deg, #f6f7fb 0%, #eef1f7 100%);
		color: #172033;
	}

	.shell {
		max-width: 1040px;
		margin: 0 auto;
		padding: 2rem 1.25rem 3rem;
	}

	header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 1rem;
		margin-bottom: 2rem;
	}

	.eyebrow {
		margin: 0 0 0.25rem;
		text-transform: uppercase;
		letter-spacing: 0.14em;
		font-size: 0.72rem;
		color: #5f708e;
	}

	h1 {
		margin: 0;
		font-size: clamp(1.8rem, 3vw, 2.4rem);
	}

	nav {
		display: flex;
		gap: 0.9rem;
		align-items: center;
		flex-wrap: wrap;
	}

	a, button {
		font: inherit;
		text-decoration: none;
		color: #172033;
		background: white;
		border: 1px solid #d6dcea;
		border-radius: 999px;
		padding: 0.55rem 0.9rem;
		cursor: pointer;
	}
</style>
