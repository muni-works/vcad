"""
Custom handler for cad0 HuggingFace Inference Endpoint.

This loads the Qwen2.5-Coder-7B-Instruct base model with the cad0 LoRA adapter.
Upload this file to the campedersen/cad0 model repo.
"""

from typing import Dict, Any
import torch
from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig


class EndpointHandler:
    def __init__(self, path: str = ""):
        """Load model and tokenizer."""
        # Base model that cad0 was fine-tuned from
        base_model = "Qwen/Qwen2.5-Coder-7B-Instruct"

        # Load tokenizer from base model
        self.tokenizer = AutoTokenizer.from_pretrained(
            base_model,
            trust_remote_code=True
        )

        # Quantization config for efficient inference
        bnb_config = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_compute_dtype=torch.float16,
        )

        # Load the fine-tuned model (path points to the model repo)
        self.model = AutoModelForCausalLM.from_pretrained(
            path,
            quantization_config=bnb_config,
            trust_remote_code=True,
            device_map="auto",
            low_cpu_mem_usage=True,
        )

        self.model.eval()

    def __call__(self, data: Dict[str, Any]) -> Dict[str, Any]:
        """
        Handle inference request.

        Expected input format:
        {
            "inputs": "prompt text or chat-formatted text",
            "parameters": {
                "max_new_tokens": 256,
                "temperature": 0.1,
                "do_sample": true,
                "return_full_text": false
            }
        }
        """
        inputs = data.get("inputs", "")
        parameters = data.get("parameters", {})

        # Default parameters
        max_new_tokens = parameters.get("max_new_tokens", 256)
        temperature = parameters.get("temperature", 0.1)
        do_sample = parameters.get("do_sample", temperature > 0)
        return_full_text = parameters.get("return_full_text", False)

        # Tokenize
        encoded = self.tokenizer(inputs, return_tensors="pt").to(self.model.device)
        input_length = encoded.input_ids.shape[1]

        # Generate
        with torch.no_grad():
            outputs = self.model.generate(
                **encoded,
                max_new_tokens=max_new_tokens,
                temperature=temperature if temperature > 0 else 1.0,
                do_sample=do_sample,
                pad_token_id=self.tokenizer.eos_token_id,
                eos_token_id=self.tokenizer.eos_token_id,
            )

        # Decode
        if return_full_text:
            generated_text = self.tokenizer.decode(outputs[0], skip_special_tokens=True)
        else:
            generated_text = self.tokenizer.decode(
                outputs[0][input_length:],
                skip_special_tokens=True
            )

        return {"generated_text": generated_text}
