FROM python:3.12-slim
RUN pip install --no-cache-dir torch torchvision --index-url https://download.pytorch.org/whl/cpu
ENV EPOCHS=2
ENV DATA_DIR=/tmp/mnist
COPY train.py /app/train.py
ENTRYPOINT ["python", "/app/train.py"]
