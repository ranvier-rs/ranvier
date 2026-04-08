<script lang="ts">
	import { goto } from '$app/navigation';
	import { login } from '$lib/api';

	let username = 'admin';
	let password = 'admin123';
	let error = '';
	let loading = false;

	async function submit() {
		loading = true;
		error = '';
		try {
			const result = await login(username, password);
			sessionStorage.setItem('reference-fullstack-admin-token', result.token);
			await goto('/');
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			loading = false;
		}
	}
</script>

<section class="login-card">
	<h2>Login</h2>
	<p>Use the demo admin credentials to access the public reference app.</p>

	<label>
		<span>Username</span>
		<input bind:value={username} />
	</label>
	<label>
		<span>Password</span>
		<input bind:value={password} type="password" />
	</label>

	<button on:click={submit} disabled={loading}>
		{#if loading}Signing in...{:else}Sign In{/if}
	</button>

	{#if error}
		<p class="error">{error}</p>
	{/if}
</section>

<style>
	.login-card {
		max-width: 440px;
		background: white;
		border: 1px solid #d6dcea;
		border-radius: 18px;
		padding: 1.25rem;
		box-shadow: 0 10px 30px rgba(45, 71, 115, 0.08);
	}

	label {
		display: grid;
		gap: 0.4rem;
		margin-top: 0.9rem;
	}

	input, button {
		font: inherit;
		padding: 0.8rem 0.9rem;
		border-radius: 12px;
		border: 1px solid #cdd5e4;
	}

	button {
		margin-top: 1rem;
		background: #1e5eff;
		color: white;
		cursor: pointer;
	}

	.error {
		color: #8d1f1f;
	}
</style>
