#!/usr/bin/env python3
"""
Ollama API integration test for mAgent
Tests AI agent functionality with local LLM model (gemma4:4b)
"""

import requests
import json

class OllamaClient:
    """Client for Ollama API"""
    
    def __init__(self, base_url="http://localhost:11434", model="gemma4:4b"):
        self.base_url = base_url
        self.model = model
    
    def generate(self, prompt, stream=False):
        """Send a prompt to Ollama and get the response"""
        url = f"{self.base_url}/api/generate"
        
        payload = {
            "model": self.model,
            "prompt": prompt,
            "stream": stream
        }
        
        try:
            response = requests.post(url, json=payload, timeout=30)
            response.raise_for_status()
            
            if stream:
                # Handle streaming response
                full_response = ""
                for line in response.iter_lines():
                    if line:
                        data = json.loads(line)
                        if "response" in data:
                            full_response += data["response"]
                        if data.get("done", False):
                            break
                return full_response
            else:
                data = response.json()
                return data.get("response", "")
                
        except requests.exceptions.RequestException as e:
            return f"Error: {str(e)}"


class SimpleAgent:
    """Simple AI agent that uses Ollama for reasoning"""
    
    def __init__(self, ollama_client):
        self.ollama = ollama_client
        self.tools = {
            "read_sensor": "Read temperature sensor",
            "write_gpio": "Control GPIO pins",
            "flash_read": "Read from flash storage",
            "flash_write": "Write to flash storage"
        }
    
    def run(self, task):
        """Run a task using Ollama"""
        print(f"\n=== Running Agent Task ===")
        print(f"Task: {task}")
        
        # Step 1: Ask Ollama to think about the task
        think_prompt = f"""You are an AI agent with these tools: {list(self.tools.keys())}.
Task: {task}.
What should you do? Respond with either 'call_tool:tool_name' or 'final_answer:your_answer'."""
        
        print(f"\nThinking...")
        response = self.ollama.generate(think_prompt)
        print(f"Ollama Response: {response}")
        
        # Step 2: Parse response and take action
        if "call_tool" in response.lower():
            # Extract tool name from response
            tool_name = None
            for tool in self.tools.keys():
                if tool in response.lower():
                    tool_name = tool
                    break
            
            if tool_name:
                print(f"\nCalling tool: {tool_name}")
                print(f"Tool description: {self.tools[tool_name]}")
                
                # Simulate tool execution
                tool_result = f"Tool {tool_name} executed successfully"
                print(f"Tool result: {tool_result}")
                
                # Step 3: Ask Ollama for final answer
                final_prompt = f"""Tool result: {tool_result}.
Task: {task}.
What is the final answer?"""
                
                print(f"\nFinal reasoning...")
                final_response = self.ollama.generate(final_prompt)
                print(f"Final answer: {final_response}")
                return final_response
            else:
                return f"Could not determine which tool to use from response: {response}"
        else:
            return response


def main():
    print("=== mAgent + Ollama Integration Test ===")
    print(f"Testing with model: gemma4:4b\n")
    
    # Initialize Ollama client
    ollama = OllamaClient(model="gemma4:4b")
    
    # Test Ollama connection
    print("Testing Ollama connection...")
    test_response = ollama.generate("Say 'Hello' in one word.")
    print(f"Connection test response: {test_response}")
    
    if "Error" in test_response:
        print("\n❌ Failed to connect to Ollama. Make sure Ollama is running:")
        print("   ollama serve")
        return
    
    # Create agent
    agent = SimpleAgent(ollama)
    
    # Test tasks
    tasks = [
        "What is the current temperature?",
        "Turn on the LED",
        "Read the configuration from flash",
        "Calculate 2 + 2",
    ]
    
    results = []
    for i, task in enumerate(tasks, 1):
        print(f"\n{'='*50}")
        print(f"Test {i}/{len(tasks)}")
        print(f"{'='*50}")
        
        try:
            result = agent.run(task)
            results.append((task, result, "Success"))
            print(f"\n✅ Task completed")
        except Exception as e:
            results.append((task, str(e), "Failed"))
            print(f"\n❌ Task failed: {e}")
    
    # Summary
    print(f"\n{'='*50}")
    print("=== Test Summary ===")
    print(f"{'='*50}")
    for task, result, status in results:
        print(f"\nTask: {task}")
        print(f"Status: {status}")
        print(f"Result: {result[:100]}..." if len(result) > 100 else f"Result: {result}")
    
    success_count = sum(1 for _, _, status in results if status == "Success")
    print(f"\nTotal: {len(results)} tests, {success_count} passed")


if __name__ == "__main__":
    main()
