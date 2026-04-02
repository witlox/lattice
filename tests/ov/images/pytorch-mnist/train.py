"""Minimal PyTorch MNIST training — CPU only, ~30s."""
import os
import torch
import torch.nn as nn
import torch.optim as optim
from torchvision import datasets, transforms

EPOCHS = int(os.environ.get("EPOCHS", "2"))
DATA_DIR = os.environ.get("DATA_DIR", "/tmp/mnist")

print(f"STARTING epochs={EPOCHS}", flush=True)


class SimpleNet(nn.Module):
    def __init__(self):
        super().__init__()
        self.fc1 = nn.Linear(784, 128)
        self.fc2 = nn.Linear(128, 10)

    def forward(self, x):
        x = x.view(-1, 784)
        x = torch.relu(self.fc1(x))
        return self.fc2(x)


transform = transforms.Compose([
    transforms.ToTensor(),
    transforms.Normalize((0.1307,), (0.3081,)),
])

train_data = datasets.MNIST(DATA_DIR, train=True, download=True, transform=transform)
test_data = datasets.MNIST(DATA_DIR, train=False, transform=transform)

train_loader = torch.utils.data.DataLoader(train_data, batch_size=256, shuffle=True)
test_loader = torch.utils.data.DataLoader(test_data, batch_size=1000)

model = SimpleNet()
optimizer = optim.Adam(model.parameters(), lr=0.001)
criterion = nn.CrossEntropyLoss()

for epoch in range(EPOCHS):
    model.train()
    total_loss = 0
    for data, target in train_loader:
        optimizer.zero_grad()
        output = model(data)
        loss = criterion(output, target)
        loss.backward()
        optimizer.step()
        total_loss += loss.item()
    avg_loss = total_loss / len(train_loader)
    print(f"EPOCH {epoch + 1}/{EPOCHS} loss={avg_loss:.4f}", flush=True)

# Evaluate
model.eval()
correct = 0
total = 0
with torch.no_grad():
    for data, target in test_loader:
        output = model(data)
        pred = output.argmax(dim=1)
        correct += (pred == target).sum().item()
        total += target.size(0)

accuracy = correct / total
print(f"RESULT accuracy={accuracy:.4f} correct={correct}/{total}", flush=True)
