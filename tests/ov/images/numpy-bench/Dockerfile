FROM python:3.12-slim
RUN pip install --no-cache-dir numpy
ENV MATRIX_SIZE=2000
COPY bench.py /app/bench.py
ENTRYPOINT ["python", "/app/bench.py"]
