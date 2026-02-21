const button = document.getElementById("place-order");
const output = document.getElementById("output");

if (button && output) {
  button.addEventListener("click", async () => {
    button.disabled = true;
    output.textContent = "Submitting order...";

    try {
      const response = await fetch("/api/order", {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          item: "embedded-demo",
          quantity: 1,
        }),
      });

      const body = await response.json();
      output.textContent = JSON.stringify(
        {
          status: response.status,
          body,
        },
        null,
        2
      );
    } catch (error) {
      output.textContent = `Request failed: ${error}`;
    } finally {
      button.disabled = false;
    }
  });
}
