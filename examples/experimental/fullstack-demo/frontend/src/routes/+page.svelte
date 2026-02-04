<script lang="ts">
    let status = "Waiting to order...";
    let loading = false;

    async function placeOrder() {
        loading = true;
        status = "Placing order...";
        try {
            // Call Ranvier Backend
            const res = await fetch("http://localhost:3030/api/order", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ item: "Ranvier t-shirt" })
            });
            if (!res.ok) {
                throw new Error(`HTTP ${res.status}`);
            }
            const data = await res.json();
            status = `Success! Response: ${JSON.stringify(data)}`;
        } catch (e) {
            status = `Error: ${e}`;
        } finally {
            loading = false;
        }
    }
</script>

<main>
    <h1>Ranvier Full-Stack Demo</h1>
    <p>This frontend runs on port 5173 (Vite).</p>
    <p>The backend runs on port 3030 (experimental Ranvier/tiny_http).</p>
    
    <div class="card">
        <button on:click={placeOrder} disabled={loading}>
            {loading ? "Ordering..." : "Place Order"}
        </button>
        <p class="status">{status}</p>
    </div>
</main>

<style>
    main {
        font-family: sans-serif;
        max-width: 800px;
        margin: 0 auto;
        padding: 2rem;
        text-align: center;
    }
    .card {
        border: 1px solid #ccc;
        padding: 2rem;
        border-radius: 8px;
        margin-top: 2rem;
        background: #f9f9f9;
        color: #333;
    }
    button {
        padding: 0.8rem 1.5rem;
        font-size: 1.2rem;
        cursor: pointer;
        background: #ff3e00;
        color: white;
        border: none;
        border-radius: 4px;
    }
    button:disabled {
        background: #ccc;
        cursor: not-allowed;
    }
    .status {
        margin-top: 1rem;
        font-weight: bold;
    }
</style>
